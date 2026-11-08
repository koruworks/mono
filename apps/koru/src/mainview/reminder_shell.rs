// SPDX-License-Identifier: MPL-2.0

//! Reminder-only chrome: the caption bell and its attention popover.
//!
//! The shell owns the bell (badge, focus, press), the popover's motion and
//! anchoring — including the 400px height clamp — the attention page (fired
//! rows, the "Next" line, the All Reminders entry), and the slide between the
//! popover's two pages. The "All Reminders" management surface the popover
//! hosts as `Page::All` lives in `reminder_management.rs`: a component any
//! host can fill a content rect with, shown here under the popover's chrome
//! (back arrow, close ×) and page slide.
//!
//! This module owns geometry and transient interaction state only. Its rows
//! are injected presentation data; it neither reads the Store nor decides
//! reminder behavior. `Presentation` is shared with the management component,
//! which borrows it for labels, ordering, and measured widths.

use std::time::Instant;

use chrono::{Datelike, Timelike};
use koru_core::time::ZoneRules;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_editor::reminders::{
    GlobalFilter, GlobalItem, GlobalItemState, GlobalManagement, Snooze, SnoozeIncrement,
};
use koru_editor::utils::when::{DueRelation, DurationUnit, ReminderDraft};
use koru_editor::utils::zone::IanaZoneRules;
use koru_i18n::{Key, Lang, t, time::LocaleFormat};
use koru_ui::popover::{KoruPopoverInspection, KoruPopoverPlacement, KoruPopoverState};
use koru_ui::{ActionButton, ActionStyle, Chip, NumericBadge, PopoverAlign, PopoverShell};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as KeyboardKey, NamedKey};

use crate::reminder_settings::{HealthAction, HealthPresentation};

use super::reminder_management::{
    ACTION_GAP, ACTION_H, Chrome, Filter, Management, OVERLAY_PAD, Request, RowAction,
};

const POPOVER_W: f32 = 360.0;
const POPOVER_MAX_H: f32 = 400.0;
const POPOVER_PAD: f32 = 12.0;
const POPOVER_HEADER_H: f32 = 26.0;
const FIRED_ROW_H: f32 = 96.0;
const QUIET_ROW_H: f32 = 44.0;
const ALL_ROW_H: f32 = 34.0;
const ACTION_KIND_COUNT: usize = 8;

/// A reversible display cap only. The full count remains in semantic state.
/// This is a settled product decision.
const BADGE_DISPLAY_CAP: usize = 99;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FiredRow {
    pub title: String,
    pub context: String,
    pub fired_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AttentionData {
    pub fired: Vec<FiredRow>,
    /// Already-formatted, cached semantic presentation data.
    pub next: Option<String>,
}

pub(super) struct Presentation {
    pub(super) schedule_labels: Vec<String>,
    row_instants: Vec<Option<i64>>,
    pub(super) ordered: [Vec<usize>; 6],
    attention_indices: Vec<usize>,
    pub(super) snooze_increment_labels: Vec<String>,
    pub(super) snooze_preview_label: Option<String>,
    bell_label: String,
    lang: Lang,
    locale: LocaleFormat,
    display_rules: IanaZoneRules,
    pub(super) action_widths: [f32; ACTION_KIND_COUNT],
    pub(super) filter_widths: [f32; 6],
    action_metric_key: Option<ActionMetricKey>,
    pub(super) banner_text_h: f32,
    banner_metric_key: Option<BannerMetricKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PresentationEnvironment {
    locale: LocaleFormat,
    display_rules: IanaZoneRules,
}

impl PresentationEnvironment {
    pub(super) fn new(locale: LocaleFormat, display_rules: IanaZoneRules) -> Self {
        Self {
            locale,
            display_rules,
        }
    }
}

#[cfg(test)]
impl From<Lang> for PresentationEnvironment {
    fn from(lang: Lang) -> Self {
        Self::new(
            LocaleFormat::for_system(Some(lang.tag()), None),
            IanaZoneRules::parse("UTC").expect("UTC rules"),
        )
    }
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            schedule_labels: Vec::new(),
            row_instants: Vec::new(),
            ordered: std::array::from_fn(|_| Vec::new()),
            attention_indices: Vec::new(),
            snooze_increment_labels: Vec::new(),
            snooze_preview_label: None,
            bell_label: String::new(),
            lang: Lang::En,
            locale: LocaleFormat::default(),
            display_rules: IanaZoneRules::parse("UTC").expect("UTC rules"),
            action_widths: [46.0; ACTION_KIND_COUNT],
            filter_widths: [64.0; 6],
            action_metric_key: None,
            banner_text_h: 0.0,
            banner_metric_key: None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ActionMetricKey {
    lang: Lang,
    font: theme::FontChoice,
    density: theme::Density,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BannerMetricKey {
    metrics: ActionMetricKey,
    width: u32,
    text: String,
}

impl ActionMetricKey {
    fn current(lang: Lang) -> Self {
        Self {
            lang,
            font: theme::ui_font(),
            density: theme::density(),
        }
    }
}

/// The feeding half of the shell's presentation contract, on the struct so
/// the Reminders page host (`reminders_view.rs`) rebuilds and measures its
/// own copy with the same rules the bell's shell keeps.
impl Presentation {
    /// Whether the display environment moved under the cached labels.
    pub(super) fn environment_changed(&self, environment: &PresentationEnvironment) -> bool {
        self.locale != environment.locale || self.display_rules != environment.display_rules
    }

    pub(super) fn set_environment(&mut self, environment: PresentationEnvironment) {
        self.lang = environment.locale.lang();
        self.locale = environment.locale;
        self.display_rules = environment.display_rules;
    }

    /// Rebuild row labels, sort keys and the per-filter ordering from the
    /// semantic rows — the half the management surface reads. The attention
    /// derivations on top of this stay with the shell.
    pub(super) fn rebuild_rows(&mut self, global: &GlobalManagement) {
        self.schedule_labels = global
            .rows
            .iter()
            .map(|row| schedule_label(&self.locale, &self.display_rules, row))
            .collect();
        self.row_instants = global
            .rows
            .iter()
            .map(|row| row_instant(row, &self.display_rules))
            .collect();
        let storage = global.rows.iter().map(schedule_storage).collect::<Vec<_>>();
        self.ordered = std::array::from_fn(|slot| {
            let selected = GlobalFilter::ALL[slot];
            let mut rows = global
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| selected.includes(row.state))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| {
                let left_row = &global.rows[*left];
                let right_row = &global.rows[*right];
                state_order(selected, left_row.state)
                    .cmp(&state_order(selected, right_row.state))
                    .then_with(|| {
                        self.row_instants[*left]
                            .unwrap_or(i64::MAX)
                            .cmp(&self.row_instants[*right].unwrap_or(i64::MAX))
                    })
                    .then_with(|| storage[*left].cmp(&storage[*right]))
                    .then_with(|| left_row.reminder_id.cmp(&right_row.reminder_id))
                    .then_with(|| left_row.target.cmp(&right_row.target))
            });
            rows
        });
    }

    /// Rebuild the snooze chooser's cached labels from the live chooser.
    pub(super) fn rebuild_snooze(&mut self, snooze: Option<&(GlobalItem, Snooze)>, now_ms: i64) {
        let lang = self.lang;
        self.snooze_increment_labels = snooze
            .map(|(_, chooser)| {
                SnoozeIncrement::ALL
                    .iter()
                    .map(|increment| {
                        snooze_increment_label(lang, *increment, chooser.count(*increment))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.snooze_preview_label = snooze.and_then(|(_, chooser)| {
            let until_ms = chooser.preview_ms(now_ms)?;
            let date = format_instant(&self.locale, &self.display_rules, until_ms);
            let until = t(lang, Key::ReminderSnoozeUntil).replacen("{}", &date, 1);
            let duration = snooze_duration_label(lang, chooser.total_minutes().unwrap_or(0));
            Some(format!("{duration} · {until}"))
        });
    }

    /// Measure the action-button and filter-chip widths for the current font,
    /// density and language. Answers whether they moved.
    pub(super) fn measure_actions(&mut self, engine: &mut TextEngine, lang: Lang) -> bool {
        let key = ActionMetricKey::current(lang);
        if self.action_metric_key == Some(key) {
            return false;
        }
        for (slot, key) in [
            Key::ReminderNotificationOpen,
            Key::ReminderSnooze,
            Key::ReminderNotificationHandled,
            Key::ReminderEdit,
            Key::ReminderPause,
            Key::ReminderResume,
            Key::ReminderDelete,
            Key::ReminderRecreate,
        ]
        .into_iter()
        .enumerate()
        {
            self.action_widths[slot] =
                ActionButton::new(t(lang, key), ActionStyle::Ghost, None).width(engine);
        }
        for (slot, filter) in Filter::ALL.into_iter().enumerate() {
            self.filter_widths[slot] = Chip::new(t(lang, filter.key()), false).width(engine);
        }
        self.action_metric_key = Some(key);
        true
    }

    /// Measure the banner's wrapped height at the host's own content width.
    /// Answers whether it moved.
    pub(super) fn measure_banner(
        &mut self,
        engine: &mut TextEngine,
        lang: Lang,
        text: String,
        panel_w: f32,
    ) -> bool {
        let text_w = (panel_w - OVERLAY_PAD * 2.0 - 16.0).max(1.0);
        let banner_key = BannerMetricKey {
            metrics: ActionMetricKey::current(lang),
            width: text_w.to_bits(),
            text,
        };
        if self.banner_metric_key.as_ref() == Some(&banner_key) {
            return false;
        }
        self.banner_text_h = if banner_key.text.is_empty() {
            0.0
        } else {
            engine
                .simple_layout_wrapped(&banner_key.text, theme::role::hint(), text_w)
                .height()
        };
        self.banner_metric_key = Some(banner_key);
        true
    }

    /// Text metrics are baked into the cached widths and banner height; the
    /// next measure recomputes them once.
    pub(super) fn invalidate_metrics(&mut self) {
        self.action_metric_key = None;
        self.banner_metric_key = None;
    }
}

/// The banner's text, for either host: a mutation failure outranks the
/// health warning.
pub(super) fn banner_text(
    lang: Lang,
    health: Option<&HealthPresentation>,
    mutation_failure: bool,
) -> String {
    if mutation_failure {
        t(lang, Key::ReminderErrorUnavailable).to_owned()
    } else {
        health
            .and_then(|health| health.banner.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    OpenAllReminders,
    SetFilter(GlobalFilter),
    Open(GlobalItem),
    Snooze(GlobalItem, i64),
    Handled(GlobalItem),
    Pause(GlobalItem),
    Resume(GlobalItem),
    Edit(GlobalItem),
    Recreate(GlobalItem),
    Delete(GlobalItem),
    ClearAllHistory,
    CancelSnooze,
    Health(HealthAction),
    RetryFailed,
    DiscardFailed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Hit {
    Bell,
    Fired(usize),
    FiredAction(usize, RowAction),
    AllReminders,
}

#[derive(Clone, Debug)]
pub(super) struct Geometry {
    bell: Rect,
    popover: Option<PopoverShell>,
    fired: Vec<Rect>,
    pub(super) fired_actions: Vec<[Rect; 3]>,
    quiet: Option<Rect>,
    all: Option<Rect>,
    focus_order: Vec<Hit>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            bell: Rect::new(0.0, 0.0, 0.0, 0.0),
            popover: None,
            fired: Vec::new(),
            fired_actions: Vec::new(),
            quiet: None,
            all: None,
            focus_order: Vec::new(),
        }
    }
}

impl Geometry {
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if self.bell.contains(x, y) {
            return Some(Hit::Bell);
        }
        for (index, actions) in self.fired_actions.iter().enumerate() {
            for (action, rect) in [RowAction::Open, RowAction::Snooze, RowAction::Handled]
                .into_iter()
                .zip(actions)
            {
                if rect.contains(x, y) {
                    return Some(Hit::FiredAction(index, action));
                }
            }
        }
        for (index, rect) in self.fired.iter().enumerate() {
            if rect.contains(x, y) {
                return Some(Hit::Fired(index));
            }
        }
        if self.all.is_some_and(|rect| rect.contains(x, y)) {
            return Some(Hit::AllReminders);
        }
        None
    }

    fn focus_order(&self) -> &[Hit] {
        &self.focus_order
    }

    fn rebuild_focus_order(&mut self) {
        self.focus_order.clear();
        if self.all.is_some() {
            self.focus_order.push(Hit::AllReminders);
        }
        for index in 0..self.fired.len() {
            self.focus_order.push(Hit::Fired(index));
            for action in [RowAction::Open, RowAction::Snooze, RowAction::Handled] {
                self.focus_order.push(Hit::FiredAction(index, action));
            }
        }
    }
}

struct PopoverMotion {
    progress: f32,
    closing: bool,
    fade: Option<(Instant, f32)>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Attention,
    All,
}

/// Horizontal travel between the two views inside the one bell popover.
/// Geometry switches to the destination immediately; only drawing travels.
struct PageMotion {
    started: Instant,
    progress: f32,
}

impl PageMotion {
    fn opening(now: Instant) -> Self {
        Self {
            started: now,
            progress: 0.0,
        }
    }

    fn advance(&mut self, now: Instant) -> bool {
        let duration = (theme::PANEL_SLIDE_MS / 1000.0).max(f32::EPSILON);
        let elapsed = (now.duration_since(self.started).as_secs_f32() / duration).clamp(0.0, 1.0);
        self.progress = koru_draw::anim::ease_out_cubic(elapsed);
        elapsed < 1.0
    }
}

impl PopoverMotion {
    fn opening(now: Instant) -> Self {
        Self {
            progress: 0.0,
            closing: false,
            fade: Some((now, 0.0)),
        }
    }

    fn close(&mut self, now: Instant) {
        if !self.closing {
            self.closing = true;
            self.fade = Some((now, self.progress));
        }
    }

    fn live(&self) -> bool {
        !self.closing
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some((started, from)) = self.fade else {
            return false;
        };
        let duration = (theme::POPOVER_FADE_MS / 1000.0).max(f32::EPSILON);
        let elapsed = now.duration_since(started).as_secs_f32() / duration;
        let elapsed = elapsed.clamp(0.0, 1.0);
        let eased = koru_draw::anim::ease_out_cubic(elapsed);
        let target = if self.closing { 0.0 } else { 1.0 };
        self.progress = from + (target - from) * eased;
        if elapsed >= 1.0 {
            self.progress = target;
            self.fade = None;
        }
        true
    }

    fn animating(&self) -> bool {
        self.fade.is_some()
    }

    fn gone(&self) -> bool {
        self.closing && self.fade.is_none() && self.progress <= f32::EPSILON
    }
}

pub(super) struct Shell {
    pub(super) attention: AttentionData,
    global: GlobalManagement,
    health: Option<HealthPresentation>,
    popover: Option<PopoverMotion>,
    page: Page,
    page_motion: Option<PageMotion>,
    hover: Option<Hit>,
    bell_pressed: bool,
    bell_focused: bool,
    focus: Option<Hit>,
    /// The "All Reminders" management surface, hosted as `Page::All`.
    pub(super) management: Management,
    pub(super) presentation: Presentation,
    geometry: Geometry,
    geometry_viewport: Option<(f32, f32)>,
    geometry_bell: Option<Rect>,
    geometry_dirty: bool,
    edit_available: bool,
    recreate_available: bool,
    mutation_failure: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            attention: AttentionData::default(),
            global: GlobalManagement::default(),
            health: None,
            popover: None,
            page: Page::Attention,
            page_motion: None,
            hover: None,
            bell_pressed: false,
            bell_focused: false,
            focus: None,
            management: Management::default(),
            presentation: Presentation::default(),
            geometry: Geometry::default(),
            geometry_viewport: None,
            geometry_bell: None,
            geometry_dirty: true,
            edit_available: false,
            recreate_available: false,
            mutation_failure: false,
        }
    }
}

impl Shell {
    #[allow(dead_code)]
    pub(super) fn set_attention(&mut self, attention: AttentionData) -> bool {
        if self.attention == attention {
            return false;
        }
        self.attention = attention;
        self.rebuild_badge();
        self.geometry_dirty = true;
        true
    }

    pub(super) fn set_global(
        &mut self,
        global: &GlobalManagement,
        environment: impl Into<PresentationEnvironment>,
    ) -> bool {
        let environment = environment.into();
        if &self.global == global && !self.presentation.environment_changed(&environment) {
            return false;
        }
        self.global = global.clone();
        self.presentation.set_environment(environment);
        self.management.sync_filter(global.filter);
        self.rebuild_presentation();
        self.geometry_dirty = true;
        true
    }

    /// Inject the wall-clock sample captured by maintenance. Draw and input
    /// use only this cached instant, so a snooze commit is exactly its preview.
    pub(super) fn set_now_ms(&mut self, now_ms: i64) -> bool {
        let changed = self.management.set_now_ms(now_ms);
        if changed {
            self.rebuild_presentation();
        }
        changed
    }

    /// Refresh wall-clock presentation and arm the next minute boundary while
    /// the chooser is live. The event loop can sleep when it is closed.
    pub(super) fn maintain_clock(&mut self, now: Instant, now_ms: i64) -> bool {
        let refresh_chooser = self.management.clock_refresh_due(now);
        let mut changed = self.set_now_ms(now_ms);
        if refresh_chooser {
            self.rebuild_snooze_presentation();
            changed = true;
        }
        self.management.arm_clock_deadline(now, now_ms);
        changed
    }

    pub(super) fn maintenance_deadline(&self) -> Option<Instant> {
        self.management.clock_deadline()
    }

    fn rebuild_presentation(&mut self) {
        let lang = self.presentation.lang;
        self.presentation.rebuild_rows(&self.global);
        self.presentation.attention_indices =
            self.presentation.ordered[filter_slot(GlobalFilter::NeedsAttention)].clone();
        self.attention.fired = self
            .presentation
            .attention_indices
            .iter()
            .map(|index| &self.global.rows[*index])
            .map(|row| FiredRow {
                title: row.title.clone(),
                context: t(lang, Key::ReminderSource).replacen("{}", &row.source_location, 1),
                fired_at: self
                    .global
                    .fired_at_ms(row)
                    .map(|at| {
                        format_instant(
                            &self.presentation.locale,
                            &self.presentation.display_rules,
                            at,
                        )
                    })
                    .unwrap_or_default(),
            })
            .collect();
        // A snoozed occurrence is still a future delivery. Keep it in the
        // Snoozed management filter, but let the compact bell's Next line
        // choose it when it precedes every ordinarily scheduled reminder.
        self.attention.next = self
            .global
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row.state,
                    GlobalItemState::ComingUp | GlobalItemState::Snoozed
                )
            })
            .min_by(|(left_index, left), (right_index, right)| {
                self.presentation.row_instants[*left_index]
                    .unwrap_or(i64::MAX)
                    .cmp(&self.presentation.row_instants[*right_index].unwrap_or(i64::MAX))
                    .then_with(|| left.reminder_id.cmp(&right.reminder_id))
                    .then_with(|| left.target.cmp(&right.target))
            })
            .and_then(|(index, _)| self.presentation.schedule_labels.get(index))
            .cloned();
        self.rebuild_badge();
        self.rebuild_snooze_presentation();
    }

    fn rebuild_badge(&mut self) {
        let count = self.attention.fired.len();
        let lang = self.presentation.lang;
        self.presentation.bell_label = if count == 0 {
            String::new()
        } else if count > BADGE_DISPLAY_CAP {
            format!(
                "{}+",
                koru_i18n::number::decimal(lang, BADGE_DISPLAY_CAP as u64)
            )
        } else {
            koru_i18n::number::decimal(lang, count as u64)
        };
    }

    fn rebuild_snooze_presentation(&mut self) {
        self.presentation
            .rebuild_snooze(self.management.snooze.as_ref(), self.management.now_ms);
    }

    pub(super) fn prepare_action_widths(&mut self, engine: &mut TextEngine, lang: Lang) -> bool {
        let mut changed = self.presentation.measure_actions(engine, lang);
        let text = banner_text(lang, self.health.as_ref(), self.mutation_failure);
        // The banner wraps for the host's actual content width, measured from
        // the rect the host last supplied; before the first maintenance the
        // popover's own width stands in.
        let panel_w = self.management.content_rect().map_or_else(
            || {
                POPOVER_W.min(
                    self.geometry_viewport
                        .map_or(POPOVER_W, |viewport| (viewport.0 - 16.0).max(1.0)),
                )
            },
            |rect| rect.w,
        );
        changed |= self
            .presentation
            .measure_banner(engine, lang, text, panel_w);
        self.geometry_dirty |= changed;
        changed
    }

    /// Publish one immutable geometry snapshot for draw and every input path.
    /// All collection growth happens here, during maintenance, never while a
    /// frame is drawn or a pointer/key event is routed. The management
    /// component's snapshot is rebuilt in the same pass, from the content
    /// rect this host computed for it.
    pub(super) fn maintain_geometry(&mut self, viewport: (f32, f32), bell: Rect) -> bool {
        if !self.geometry_dirty
            && self.geometry_viewport == Some(viewport)
            && self.geometry_bell == Some(bell)
            && !self.management.geometry_dirty
        {
            return false;
        }
        self.geometry = self.build_geometry(viewport, bell);
        let content = if self.page == Page::All {
            self.geometry.popover.map(|shell| shell.rect)
        } else {
            None
        };
        self.management.rebuild_geometry(
            content,
            Chrome::Popover,
            &self.global,
            &self.presentation,
            self.health.as_ref(),
            self.mutation_failure,
            self.edit_available,
            self.recreate_available,
        );
        self.geometry_viewport = Some(viewport);
        self.geometry_bell = Some(bell);
        self.geometry_dirty = false;
        true
    }

    #[cfg(test)]
    pub(super) fn geometry(&mut self, viewport: (f32, f32), bell: Rect) -> &Geometry {
        self.maintain_geometry(viewport, bell);
        &self.geometry
    }

    /// Text metrics are baked into the cached action geometry. The view's
    /// ordinary invalidate path calls this for font, density, and theme-role
    /// changes, so the next maintenance pass measures once before drawing.
    pub(super) fn invalidate_metrics(&mut self) {
        self.presentation.invalidate_metrics();
        self.geometry_dirty = true;
    }

    /// Edit stays disabled until the expanded-editor branch registers its
    /// consumer. That avoids an enabled navigation-only no-op while retaining
    /// the exact durable handoff for the integration slice.
    #[allow(dead_code)]
    pub(super) fn set_edit_available(&mut self, available: bool) -> bool {
        if self.edit_available == available {
            return false;
        }
        self.edit_available = available;
        self.geometry_dirty = true;
        true
    }

    /// Recreate stays hidden until the shared creation host can consume the
    /// old draft and run its passed-time proposal flow. Availability exposes
    /// the action; it never creates directly from History.
    #[allow(dead_code)]
    pub(super) fn set_recreate_available(&mut self, available: bool) -> bool {
        if self.recreate_available == available {
            return false;
        }
        self.recreate_available = available;
        self.geometry_dirty = true;
        true
    }

    /// Put the keyboard on the bell — **one stop of `F6`'s rotation**, not all
    /// of it.
    ///
    /// `MainView::focus_chrome` decides which stop this press is for; this
    /// answers only for the bell, and `false` means it already had it.
    pub(super) fn focus_bell(&mut self) -> bool {
        if self.bell_focused {
            return false;
        }
        self.bell_focused = true;
        self.focus = None;
        self.management.focus = None;
        true
    }

    /// Give the keyboard back, because `F6` moved on to another stop.
    ///
    /// The mouse already does this from three places in `mouse_down`; this is
    /// the same clearing reached by name, so the rotation cannot leave two
    /// stops looking focused at once.
    pub(super) fn blur_bell(&mut self) -> bool {
        if !self.bell_focused {
            return false;
        }
        self.bell_focused = false;
        true
    }

    /// Whether the bell has the keyboard, which is what its ring is drawn from
    /// — and where `F6`'s rotation reads which stop it is leaving.
    pub(super) fn bell_is_focused(&self) -> bool {
        self.bell_focused
    }

    pub(super) fn set_health(&mut self, health: HealthPresentation) -> bool {
        if self.health.as_ref() == Some(&health) {
            return false;
        }
        self.health = Some(health);
        self.geometry_dirty = true;
        true
    }

    pub(super) fn set_mutation_failure(&mut self, failed: bool) -> bool {
        if self.mutation_failure == failed {
            return false;
        }
        self.mutation_failure = failed;
        self.geometry_dirty = true;
        true
    }

    pub(super) fn open_pending_snooze(
        &mut self,
        request: Option<&super::reminders::SnoozeRequest>,
        now: Instant,
    ) -> bool {
        let Some(request) = request else {
            return false;
        };
        let reminder = request.reminder.to_storage();
        let occurrence = request.occurrence.to_storage();
        let Some(row) = self
            .global
            .rows
            .iter()
            .find(|row| {
                row.reminder_id == reminder && row.occurrence.as_deref() == Some(&occurrence)
            })
            .cloned()
        else {
            return false;
        };
        if self
            .management
            .snooze
            .as_ref()
            .is_some_and(|(open, _)| open == &row)
        {
            return false;
        }
        self.show_all(now);
        self.management.open_snooze(row, now);
        tracing::info!(
            target: "koru::notification_activation",
            reminder = reminder,
            occurrence = occurrence,
            "Koru opened the snooze duration picker for the notification occurrence"
        );
        true
    }

    pub(super) fn reset_profile(&mut self) {
        self.attention = AttentionData::default();
        self.global = GlobalManagement::default();
        self.health = None;
        self.popover = None;
        self.page = Page::Attention;
        self.page_motion = None;
        self.hover = None;
        self.bell_pressed = false;
        self.bell_focused = false;
        self.focus = None;
        self.management = Management::default();
        self.presentation = Presentation::default();
        self.geometry = Geometry::default();
        self.geometry_viewport = None;
        self.geometry_bell = None;
        self.geometry_dirty = true;
        self.mutation_failure = false;
    }

    pub(super) fn animating(&self) -> bool {
        self.popover.as_ref().is_some_and(PopoverMotion::animating) || self.page_motion.is_some()
    }

    pub(super) fn advance(&mut self, now: Instant) {
        if let Some(popover) = self.popover.as_mut() {
            popover.advance(now);
            if popover.gone() {
                self.popover = None;
                self.page = Page::Attention;
                self.page_motion = None;
            }
        }
        if self
            .page_motion
            .as_mut()
            .is_some_and(|motion| !motion.advance(now))
        {
            self.page_motion = None;
        }
    }

    pub(super) fn bell_label(&self, _lang: Lang) -> &str {
        &self.presentation.bell_label
    }

    /// How many fired reminders are asking for attention — the number the
    /// badge carries, and the one the tasks rail's Reminders row repeats.
    pub(super) fn attention_count(&self) -> usize {
        self.attention.fired.len()
    }

    pub(super) fn popover_live(&self) -> bool {
        self.page == Page::Attention && self.popover.as_ref().is_some_and(PopoverMotion::live)
    }

    #[cfg(test)]
    pub(super) fn overlay_visible(&self) -> bool {
        self.page == Page::All && self.popover.is_some()
    }

    /// Whether the overlay owns input. Closing makes it deaf immediately even
    /// though the elapsed-time slide remains visible until it reaches zero.
    pub(super) fn overlay_live(&self) -> bool {
        self.page == Page::All && self.popover.as_ref().is_some_and(PopoverMotion::live)
    }

    /// This surface overlays the document; it never contributes layout width.
    #[cfg(test)]
    pub(super) const fn document_inset(&self) -> f32 {
        0.0
    }

    pub(super) fn draw_bell(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        bell: Rect,
        hovered: bool,
        lang: Lang,
    ) {
        let chip = koru_ui::icon_button::hover_chip(bell);
        if self.bell_pressed {
            scene::fill(
                canvas,
                root,
                theme::p().accent.multiply_alpha(0.12),
                &scene::rrect(chip.x, chip.y, chip.w, chip.h, 6.0),
            );
        }
        koru_ui::icon_button::draw(
            canvas,
            root,
            engine,
            bell,
            icons::BELL,
            koru_ui::icon_button::State {
                hovered: hovered || self.bell_pressed,
                active: false,
                disabled: false,
            },
        );
        if self.bell_focused {
            scene::stroke(
                canvas,
                root,
                theme::p().focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(chip.x - 1.0, chip.y - 1.0, chip.w + 2.0, chip.h + 2.0, 7.0),
            );
        }
        if let Some(badge) = NumericBadge::measure(engine, bell, self.bell_label(lang)) {
            badge.draw(canvas, root);
        }
    }

    pub(super) fn draw_surfaces(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        _viewport: (f32, f32),
        _bell: Rect,
        lang: Lang,
    ) {
        let geometry = &self.geometry;
        if let (Some(motion), Some(shell)) = (self.popover.as_ref(), geometry.popover) {
            let rise = -(1.0 - motion.progress) * theme::POPOVER_RISE;
            let transform = root * Affine::translate((0.0, f64::from(rise)));
            let halo = Rect::new(
                shell.rect.x - 24.0,
                shell.rect.y - 24.0,
                shell.rect.w + 48.0,
                shell.rect.h + 48.0,
            );
            let _fade_clip = (motion.progress < 1.0).then(|| {
                koru_draw::inspect::clip(transform, scene::rect(halo.x, halo.y, halo.w, halo.h))
            });
            if motion.progress < 1.0 {
                scene::fade_layer(
                    canvas,
                    transform,
                    motion.progress,
                    &scene::rect(halo.x, halo.y, halo.w, halo.h),
                );
            }
            shell.draw_with_content(canvas, transform, KoruPopoverInspection {
                state: if motion.closing { KoruPopoverState::Closing } else if motion.fade.is_some() { KoruPopoverState::Opening } else { KoruPopoverState::Open },
                placement: KoruPopoverPlacement::Anchored { anchor: geometry.bell, align: PopoverAlign::End },
                ..Default::default()
            }, |canvas, transform| {
                koru_draw::inspect::current(|r| {
                    r.property("host association", "reminder bell", "Shell::build_geometry bell anchor; source association, not occurrence identity");
                    r.property("recipe body width", POPOVER_W, "reminder_shell::POPOVER_W; domain recipe");
                    r.property("recipe maximum height", POPOVER_MAX_H, "reminder_shell::POPOVER_MAX_H; domain recipe");
                    r.property("recipe body padding", POPOVER_PAD, "reminder_shell::POPOVER_PAD; domain recipe");
                    r.property("recipe header height", POPOVER_HEADER_H, "reminder_shell::POPOVER_HEADER_H; domain recipe");
                    r.property("fade progress", motion.progress, "PopoverMotion::progress");
                    r.property("vertical rise", rise, "PopoverMotion::progress and theme::POPOVER_RISE");
                    r.property("page", format!("{:?}", self.page), "Shell::page");
                    if let Some(page_motion) = &self.page_motion {
                        r.property("page slide progress", page_motion.progress, "PageMotion::progress");
                    }
                });
                canvas.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    transform,
                    &scene::rect(shell.rect.x, shell.rect.y, shell.rect.w, shell.rect.h),
                );
                let _clip = koru_draw::inspect::clip(transform, scene::rect(shell.rect.x, shell.rect.y, shell.rect.w, shell.rect.h));
                if let Some(page_motion) = &self.page_motion {
                    let distance = shell.rect.w * page_motion.progress;
                    self.draw_attention(
                        canvas,
                        transform * Affine::translate((f64::from(-distance), 0.0)),
                        engine,
                        geometry,
                        lang,
                    );
                    self.draw_management(
                        canvas,
                        transform * Affine::translate((f64::from(shell.rect.w - distance), 0.0)),
                        engine,
                        lang,
                    );
                } else if self.page == Page::All {
                    self.draw_management(canvas, transform, engine, lang);
                } else {
                    self.draw_attention(canvas, transform, engine, geometry, lang);
                }
                canvas.pop_layer();
            });
            if motion.progress < 1.0 {
                canvas.pop_layer();
            }
        }
    }

    /// The management surface, drawn under the caller's translate. The page
    /// slide stays here in the popover host; the component only fills a rect.
    fn draw_management(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        lang: Lang,
    ) {
        let _content = self.geometry.popover.map(|shell| {
            koru_draw::inspect::scope(
                "Reminder management content",
                "unregistered",
                root,
                scene::rect(shell.rect.x, shell.rect.y, shell.rect.w, shell.rect.h),
                |_| {},
            )
        });
        self.management.draw(
            canvas,
            root,
            engine,
            lang,
            &self.global,
            &self.presentation,
            self.health.as_ref(),
            self.mutation_failure,
        );
    }

    fn draw_attention(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        geometry: &Geometry,
        lang: Lang,
    ) {
        let Some(shell) = geometry.popover else {
            return;
        };
        let _content = koru_draw::inspect::scope(
            "Reminder attention content",
            "unregistered",
            root,
            scene::rect(shell.rect.x, shell.rect.y, shell.rect.w, shell.rect.h),
            |_| {},
        );
        draw_text(
            canvas,
            root,
            engine,
            t(lang, Key::ReminderAttention),
            shell.rect.x + POPOVER_PAD,
            shell.rect.y + 9.0,
            theme::role::section_heading(),
            theme::p().text,
        );
        for (index, (row, rect)) in self.attention.fired.iter().zip(&geometry.fired).enumerate() {
            if self.hover == Some(Hit::Fired(index)) || self.focus == Some(Hit::Fired(index)) {
                scene::fill(
                    canvas,
                    root,
                    theme::p().hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            draw_text(
                canvas,
                root,
                engine,
                &row.title,
                rect.x + 8.0,
                rect.y + 6.0,
                theme::role::row(),
                theme::p().text,
            );
            draw_text(
                canvas,
                root,
                engine,
                &row.context,
                rect.x + 8.0,
                rect.y + 27.0,
                theme::role::meta(),
                theme::p().text_dim,
            );
            if !row.fired_at.is_empty() {
                draw_text(
                    canvas,
                    root,
                    engine,
                    &row.fired_at,
                    rect.x + 8.0,
                    rect.y + 47.0,
                    theme::role::hint(),
                    theme::p().row_hint,
                );
            }
            if let Some(actions) = geometry.fired_actions.get(index) {
                for ((label, action), action_rect) in [
                    (Key::ReminderNotificationOpen, RowAction::Open),
                    (Key::ReminderSnooze, RowAction::Snooze),
                    (Key::ReminderNotificationHandled, RowAction::Handled),
                ]
                .into_iter()
                .zip(actions)
                {
                    let style = match action {
                        RowAction::Handled => ActionStyle::Primary,
                        RowAction::Snooze => ActionStyle::Secondary,
                        RowAction::Open => ActionStyle::Ghost,
                        _ => ActionStyle::Ghost,
                    };
                    ActionButton::new(t(lang, label), style, None).draw(
                        canvas,
                        root,
                        engine,
                        *action_rect,
                        self.hover == Some(Hit::FiredAction(index, action))
                            || self.focus == Some(Hit::FiredAction(index, action)),
                    );
                }
            }
        }
        if let Some(rect) = geometry.quiet {
            draw_text(
                canvas,
                root,
                engine,
                t(lang, Key::ReminderNext),
                rect.x + 8.0,
                rect.y + 3.0,
                theme::role::meta(),
                theme::p().row_hint,
            );
            let value = self
                .attention
                .next
                .as_deref()
                .unwrap_or_else(|| t(lang, Key::ReminderNoAttention));
            draw_text(
                canvas,
                root,
                engine,
                value,
                rect.x + 8.0,
                rect.y + 21.0,
                theme::role::row(),
                theme::p().text_dim,
            );
        }
        if let Some(rect) = geometry.all {
            ActionButton::new(
                t(lang, Key::ReminderAll),
                ActionStyle::Ghost,
                Some(icons::CARET_RIGHT),
            )
            .draw(
                canvas,
                root,
                engine,
                rect,
                self.hover == Some(Hit::AllReminders) || self.focus == Some(Hit::AllReminders),
            );
        }
    }

    fn cached_hit(&self, bell: Rect, x: f32, y: f32) -> Option<Hit> {
        // The caption slot is arithmetic supplied by the current layout. Test
        // it directly before first maintenance and directly after relayout;
        // this path neither builds nor clones geometry.
        if bell.contains(x, y) {
            return Some(Hit::Bell);
        }
        let local_x = x - self.page_slide_offset();
        match self.geometry.hit(local_x, y) {
            Some(Hit::Bell) => None,
            hit => hit,
        }
    }

    fn page_slide_offset(&self) -> f32 {
        self.page_motion
            .as_ref()
            .zip(self.geometry.popover)
            .map_or(0.0, |(motion, shell)| {
                (1.0 - motion.progress) * shell.rect.w
            })
    }

    pub(super) fn pointer_move(
        &mut self,
        _viewport: (f32, f32),
        bell: Rect,
        x: f32,
        y: f32,
    ) -> bool {
        let mut changed = false;
        if self.overlay_live() {
            let hover = bell.contains(x, y).then_some(Hit::Bell);
            changed |= self.hover != hover;
            self.hover = hover;
            let local_x = x - self.page_slide_offset();
            let hit = self.management.hit(local_x, y);
            changed |= self.management.set_hover(hit);
        } else if self.popover_live() {
            let hit = self.cached_hit(bell, x, y);
            changed |= self.hover != hit;
            self.hover = hit;
            changed |= self.management.set_hover(None);
        } else {
            let hover = bell.contains(x, y).then_some(Hit::Bell);
            changed |= self.hover != hover;
            self.hover = hover;
            changed |= self.management.set_hover(None);
        }
        changed
    }

    pub(super) fn mouse_down(
        &mut self,
        _viewport: (f32, f32),
        bell: Rect,
        x: f32,
        y: f32,
        now: Instant,
    ) -> (bool, Option<Intent>) {
        if self.overlay_live() {
            let bell_hit = bell.contains(x, y);
            self.bell_focused = bell_hit;
            if bell_hit {
                self.close_overlay(now);
                return (true, None);
            }
            let local_x = x - self.page_slide_offset();
            let (intent, request) =
                self.management
                    .mouse_down(local_x, y, now, &self.global, self.health.as_ref());
            self.handle_request(request, now);
            return (true, intent);
        }
        let hit = self.cached_hit(bell, x, y);
        if hit == Some(Hit::Bell) {
            self.bell_pressed = true;
            self.bell_focused = true;
            self.toggle_popover(now);
            return (true, None);
        }
        self.bell_focused = false;
        if self.popover_live() {
            match hit {
                Some(Hit::AllReminders) => return (true, Some(Intent::OpenAllReminders)),
                Some(Hit::Fired(index)) => {
                    return (true, self.attention_row(index).cloned().map(Intent::Open));
                }
                Some(Hit::FiredAction(index, action)) => {
                    if let Some(row) = self.attention_row(index).cloned() {
                        if action == RowAction::Snooze && row.occurrence.is_some() {
                            self.show_all(now);
                        }
                        return (true, self.management.row_action(row, action, now));
                    }
                    return (true, None);
                }
                _ => {
                    self.close_popover(now);
                    return (true, None);
                }
            }
        }
        (false, None)
    }

    pub(super) fn mouse_up(&mut self) -> bool {
        let changed = self.bell_pressed;
        self.bell_pressed = false;
        changed
    }

    pub(super) fn key(
        &mut self,
        _viewport: (f32, f32),
        _bell: Rect,
        key: &KeyboardKey,
        now: Instant,
    ) -> (bool, Option<Intent>) {
        if self.overlay_live() {
            let (intent, request) = self.management.key(
                key,
                now,
                &self.global,
                &self.presentation,
                self.health.as_ref(),
            );
            self.handle_request(request, now);
            return (true, intent);
        }
        if self.popover_live() {
            match key {
                KeyboardKey::Named(NamedKey::Escape) => self.close_popover(now),
                KeyboardKey::Named(NamedKey::ArrowUp) => {
                    self.move_focus_cached(-1);
                }
                KeyboardKey::Named(NamedKey::ArrowDown | NamedKey::Tab) => {
                    self.move_focus_cached(1);
                }
                KeyboardKey::Named(NamedKey::Enter | NamedKey::Space) => {
                    if let Some(focus) = self.focus {
                        return (true, self.activate(focus, now));
                    }
                }
                _ => return (false, None),
            }
            return (true, None);
        }
        if self.bell_focused && matches!(key, KeyboardKey::Named(NamedKey::Enter | NamedKey::Space))
        {
            self.toggle_popover(now);
            return (true, None);
        }
        (false, None)
    }

    pub(super) fn apply(&mut self, intent: Intent, now: Instant) {
        match intent {
            Intent::OpenAllReminders => {
                self.show_all(now);
                self.management.apply_open_all();
            }
            Intent::SetFilter(filter) => self.management.apply_filter(filter),
            Intent::Open(_) | Intent::Edit(_) | Intent::Recreate(_) => {
                self.close_popover(now);
                self.close_overlay(now);
            }
            Intent::Snooze(_, _)
            | Intent::Handled(_)
            | Intent::Pause(_)
            | Intent::Resume(_)
            | Intent::Delete(_)
            | Intent::ClearAllHistory
            | Intent::CancelSnooze
            | Intent::Health(_) => {}
            Intent::RetryFailed | Intent::DiscardFailed => self.mutation_failure = false,
        }
    }

    pub(super) fn cursor_press(&self, _viewport: (f32, f32), bell: Rect, x: f32, y: f32) -> bool {
        if bell.contains(x, y) {
            return true;
        }
        let local_x = x - self.page_slide_offset();
        if self.management.cursor_press(local_x, y) {
            return true;
        }
        matches!(
            self.geometry.hit(local_x, y),
            Some(Hit::AllReminders | Hit::FiredAction(_, _))
        )
    }

    /// A secondary press is a dismissal gesture for either reminder surface,
    /// and is consumed so the hidden document cannot open its context menu.
    pub(super) fn context_press(
        &mut self,
        _viewport: (f32, f32),
        bell: Rect,
        x: f32,
        y: f32,
        now: Instant,
    ) -> bool {
        if self.overlay_live() {
            let local_x = x - self.page_slide_offset();
            if !self.management.contains(local_x, y) {
                self.close_overlay(now);
            }
            return true;
        }
        if self.popover_live() {
            self.close_popover(now);
            return true;
        }
        self.cached_hit(bell, x, y) == Some(Hit::Bell)
    }

    pub(super) fn blocks_pointer(
        &self,
        _viewport: (f32, f32),
        _bell: Rect,
        x: f32,
        y: f32,
    ) -> bool {
        if self.overlay_live() {
            return true;
        }
        self.popover_live()
            && self
                .geometry
                .popover
                .is_some_and(|shell| shell.contains(x, y))
    }

    fn toggle_popover(&mut self, now: Instant) {
        if self.popover.as_ref().is_some_and(PopoverMotion::live) {
            self.close_popover(now);
        } else {
            self.page = Page::Attention;
            self.page_motion = None;
            self.popover = Some(PopoverMotion::opening(now));
            self.focus = None;
            self.management.focus = None;
            self.geometry_dirty = true;
        }
    }

    fn close_popover(&mut self, now: Instant) {
        if let Some(popover) = self.popover.as_mut() {
            popover.close(now);
        }
        self.hover = None;
        self.focus = None;
        self.management.clear_hover_focus();
        self.geometry_dirty = true;
    }

    fn show_all(&mut self, now: Instant) {
        let slide =
            self.page == Page::Attention && self.popover.as_ref().is_some_and(PopoverMotion::live);
        if !self.popover.as_ref().is_some_and(PopoverMotion::live) {
            self.popover = Some(PopoverMotion::opening(now));
        }
        self.page = Page::All;
        self.page_motion = slide.then(|| PageMotion::opening(now));
        self.hover = None;
        self.focus = None;
        self.management.clear_hover_focus();
        self.geometry_dirty = true;
    }

    fn show_attention(&mut self) {
        self.page = Page::Attention;
        self.page_motion = None;
        self.management.leave();
        self.hover = None;
        self.focus = None;
        self.geometry_dirty = true;
    }

    fn close_overlay(&mut self, now: Instant) {
        self.close_popover(now);
        self.management.dismiss();
        self.geometry_dirty = true;
    }

    /// Navigation only the host can perform: the component asks, the popover
    /// turns the page or dismisses itself.
    fn handle_request(&mut self, request: Option<Request>, now: Instant) {
        match request {
            Some(Request::Back) => self.show_attention(),
            Some(Request::Close) => self.close_overlay(now),
            None => {}
        }
    }

    fn move_focus_cached(&mut self, delta: isize) {
        let order = self.geometry.focus_order();
        if order.is_empty() {
            self.focus = None;
            return;
        }
        let at = self
            .focus
            .and_then(|focus| order.iter().position(|candidate| *candidate == focus))
            .map_or(if delta < 0 { order.len() } else { usize::MAX }, |at| at);
        let next = if at == usize::MAX {
            0
        } else {
            (at as isize + delta).rem_euclid(order.len() as isize) as usize
        };
        self.focus = Some(order[next]);
    }

    fn activate(&mut self, hit: Hit, now: Instant) -> Option<Intent> {
        match hit {
            Hit::Fired(index) => self.attention_row(index).cloned().map(Intent::Open),
            Hit::FiredAction(index, action) => {
                let row = self.attention_row(index).cloned()?;
                if action == RowAction::Snooze && row.occurrence.is_some() {
                    self.show_all(now);
                }
                self.management.row_action(row, action, now)
            }
            Hit::AllReminders => Some(Intent::OpenAllReminders),
            Hit::Bell => None,
        }
    }

    fn attention_row(&self, index: usize) -> Option<&GlobalItem> {
        self.presentation
            .attention_indices
            .get(index)
            .and_then(|index| self.global.rows.get(*index))
    }

    #[cfg(test)]
    fn ordered_indices(&self, selected: GlobalFilter) -> &[usize] {
        &self.presentation.ordered[filter_slot(selected)]
    }

    pub(super) fn scroll_by(&mut self, _viewport: (f32, f32), delta: f32) -> bool {
        if !self.overlay_live() {
            return false;
        }
        self.management.scroll_by(delta, &self.presentation)
    }

    fn build_geometry(&self, viewport: (f32, f32), bell: Rect) -> Geometry {
        let viewport_rect = Rect::new(0.0, 0.0, viewport.0.max(0.0), viewport.1.max(0.0));
        let popover = self.popover.as_ref().map(|_| {
            let width = POPOVER_W.min((viewport.0 - 16.0).max(1.0));
            let height = if self.page == Page::All {
                (viewport.1 - bell.y - bell.h - 14.0).max(1.0)
            } else {
                let content_h = QUIET_ROW_H + self.attention.fired.len() as f32 * FIRED_ROW_H;
                POPOVER_PAD + POPOVER_HEADER_H + content_h + ALL_ROW_H + POPOVER_PAD
            };
            let height = height.min(POPOVER_MAX_H).min((viewport.1 - 16.0).max(1.0));
            PopoverShell::anchored(bell, viewport_rect, width, height, PopoverAlign::End)
        });
        let mut fired = Vec::new();
        let mut fired_actions = Vec::new();
        let mut quiet = None;
        let mut all = None;
        if let Some(shell) = popover {
            let mut y = shell.rect.y + POPOVER_PAD + POPOVER_HEADER_H;
            all = Some(Rect::new(
                shell.rect.x + POPOVER_PAD,
                y,
                shell.rect.w - POPOVER_PAD * 2.0,
                ALL_ROW_H,
            ));
            y += ALL_ROW_H;
            quiet = Some(Rect::new(
                shell.rect.x + POPOVER_PAD,
                y,
                shell.rect.w - POPOVER_PAD * 2.0,
                QUIET_ROW_H,
            ));
            y += QUIET_ROW_H;
            let available = (shell.rect.y + shell.rect.h - POPOVER_PAD - y).max(0.0);
            let visible = self
                .attention
                .fired
                .len()
                .min((available / FIRED_ROW_H).floor() as usize);
            for _ in self.attention.fired.iter().take(visible) {
                let row = Rect::new(
                    shell.rect.x + POPOVER_PAD,
                    y,
                    shell.rect.w - POPOVER_PAD * 2.0,
                    FIRED_ROW_H,
                );
                let kinds = [RowAction::Open, RowAction::Snooze, RowAction::Handled];
                let mut actions = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
                let mut right = row.x + row.w - 8.0;
                for index in (0..kinds.len()).rev() {
                    let width = self.presentation.action_widths[kinds[index].width_slot(false)];
                    let x = right - width;
                    actions[index] = Rect::new(x, row.y + row.h - ACTION_H - 6.0, width, ACTION_H);
                    right = x - ACTION_GAP;
                }
                fired_actions.push(actions);
                fired.push(row);
                y += FIRED_ROW_H;
            }
        }
        let mut geometry = Geometry {
            bell,
            popover,
            fired,
            fired_actions,
            quiet,
            all,
            focus_order: Vec::new(),
        };
        geometry.rebuild_focus_order();
        geometry
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_text(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    color: vello::peniko::Color,
) {
    let layout = engine.simple_layout(value, style);
    scene::draw_layout(canvas, root, &layout, x, y, Some(color));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_wrapped_text(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    width: f32,
    style: theme::TextStyle,
    color: vello::peniko::Color,
) {
    let layout = engine.simple_layout_wrapped(value, style, width.max(1.0));
    scene::draw_layout(canvas, root, &layout, x, y, Some(color));
}

fn schedule_label(
    locale: &LocaleFormat,
    display_rules: &IanaZoneRules,
    row: &GlobalItem,
) -> String {
    if row.state == GlobalItemState::Snoozed
        && let Some(until_ms) = row.sort_at_ms
    {
        return format_instant(locale, display_rules, until_ms);
    }
    let lang = locale.lang();
    match &row.draft.schedule {
        ReminderDraft::Floating(wall) => format_wall(locale, wall.wall()),
        ReminderDraft::Zoned(value) => format!(
            "{} \u{00b7} {}",
            format_instant(locale, display_rules, value.instant_ms()),
            display_rules.zone()
        ),
        ReminderDraft::Relative {
            relation,
            offset,
            preview,
            ..
        } => {
            if let Some(preview) = preview {
                return match preview {
                    koru_editor::utils::when::ReminderMoment::Floating(wall) => {
                        format_wall(locale, wall.wall())
                    }
                    koru_editor::utils::when::ReminderMoment::Zoned(value) => {
                        format_instant(locale, display_rules, value.instant_ms())
                    }
                };
            }
            match (relation, offset) {
                (DueRelation::At, _) | (_, None) => t(lang, Key::ReminderAtDueDate).to_owned(),
                (DueRelation::Before, Some(offset)) => {
                    let key = match offset.unit {
                        DurationUnit::Minute => Key::ReminderSnoozeMinutes,
                        DurationUnit::Hour => Key::ReminderSnoozeHours,
                        DurationUnit::Day => Key::ReminderDurationDays,
                        DurationUnit::Week => Key::ReminderDurationWeeks,
                        DurationUnit::Month => Key::ReminderDurationMonths,
                    };
                    let amount = koru_i18n::number::decimal(lang, u64::from(offset.amount));
                    let duration = t(lang, key).replacen("{}", &amount, 1);
                    t(lang, Key::ReminderBeforeDue).replace("{}", &duration)
                }
            }
        }
    }
}

fn format_wall(locale: &LocaleFormat, wall: chrono::NaiveDateTime) -> String {
    let parts = koru_i18n::time::DateTimeParts {
        year: wall.year(),
        month: wall.month() as u8,
        day: wall.day() as u8,
        hour: wall.hour() as u8,
        minute: wall.minute() as u8,
        second: wall.second() as u8,
    };
    locale.date_time(parts)
}

fn format_instant(locale: &LocaleFormat, display_rules: &IanaZoneRules, instant_ms: i64) -> String {
    format_wall(locale, display_rules.wall_of(instant_ms))
}

fn snooze_duration_label(lang: Lang, total_minutes: u32) -> String {
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    let mut parts = Vec::with_capacity(2);
    if hours > 0 {
        let amount = koru_i18n::number::decimal(lang, u64::from(hours));
        parts.push(t(lang, Key::ReminderSnoozeHours).replacen("{}", &amount, 1));
    }
    if minutes > 0 || hours == 0 {
        let amount = koru_i18n::number::decimal(lang, u64::from(minutes));
        parts.push(t(lang, Key::ReminderSnoozeMinutes).replacen("{}", &amount, 1));
    }
    parts.join(" ")
}

fn snooze_increment_label(lang: Lang, increment: SnoozeIncrement, count: u32) -> String {
    let (key, amount) = if increment.minutes() == 60 {
        (Key::ReminderSnoozeHours, 1)
    } else {
        (Key::ReminderSnoozeMinutes, increment.minutes())
    };
    let amount = koru_i18n::number::decimal(lang, u64::from(amount));
    let count = koru_i18n::number::decimal(lang, u64::from(count));
    let base = t(lang, key).replacen("{}", &amount, 1);
    format!("{base} ×{count}")
}

fn row_instant(row: &GlobalItem, display_rules: &IanaZoneRules) -> Option<i64> {
    row.sort_at_ms.or_else(|| match &row.draft.schedule {
        ReminderDraft::Floating(wall) => Some(display_rules.instant_of(wall.wall())),
        ReminderDraft::Zoned(value) => Some(value.instant_ms()),
        ReminderDraft::Relative { preview, .. } => {
            preview.as_ref().and_then(|moment| match moment {
                koru_editor::utils::when::ReminderMoment::Floating(wall) => {
                    Some(display_rules.instant_of(wall.wall()))
                }
                koru_editor::utils::when::ReminderMoment::Zoned(value) => Some(value.instant_ms()),
            })
        }
    })
}

fn schedule_storage(row: &GlobalItem) -> String {
    match &row.draft.schedule {
        ReminderDraft::Floating(wall) => wall.to_storage(),
        ReminderDraft::Zoned(value) => format!("{:020}", value.instant_ms()),
        ReminderDraft::Relative { preview, .. } => {
            preview
                .as_ref()
                .map_or_else(String::new, |moment| match moment {
                    koru_editor::utils::when::ReminderMoment::Floating(wall) => wall.to_storage(),
                    koru_editor::utils::when::ReminderMoment::Zoned(value) => {
                        format!("{:020}", value.instant_ms())
                    }
                })
        }
    }
}

fn state_order(filter: GlobalFilter, state: GlobalItemState) -> u8 {
    if filter != GlobalFilter::All {
        return 0;
    }
    match state {
        GlobalItemState::NeedsAttention => 0,
        GlobalItemState::ComingUp => 1,
        GlobalItemState::Snoozed => 2,
        GlobalItemState::Paused => 3,
        GlobalItemState::History => 4,
    }
}

pub(super) const fn filter_slot(filter: GlobalFilter) -> usize {
    match filter {
        GlobalFilter::All => 0,
        GlobalFilter::NeedsAttention => 1,
        GlobalFilter::ComingUp => 2,
        GlobalFilter::Snoozed => 3,
        GlobalFilter::Paused => 4,
        GlobalFilter::History => 5,
    }
}

/// What a row's projected state is called.
///
/// `pub(super)` since Plan 2.2: the accessibility projection says the same
/// word about the same state, and a second table beside this one is how a
/// screen reader and the overlay start disagreeing about what *Snoozed* means.
pub(super) fn state_key(state: GlobalItemState) -> Key {
    match state {
        GlobalItemState::NeedsAttention => Key::ReminderNeedsAttention,
        GlobalItemState::ComingUp => Key::ReminderComingUp,
        GlobalItemState::Snoozed => Key::ReminderFilterSnoozed,
        GlobalItemState::Paused => Key::ReminderFilterPaused,
        GlobalItemState::History => Key::ReminderHistory,
    }
}
#[cfg(test)]
mod inspection_tests;

#[cfg(test)]
pub(super) mod tests {
    use std::time::Duration;

    use super::*;
    use chrono::NaiveDate;
    use koru_core::time::FloatingDateTime;
    use koru_editor::reminders::{Draft, GlobalAttention};

    use crate::mainview::reminder_management::Filter;

    pub(crate) const VIEWPORT: (f32, f32) = (900.0, 560.0);
    pub(crate) fn bell() -> Rect {
        Rect::new(800.0, 0.0, 36.0, 36.0)
    }

    pub(crate) fn opened_popover(now: Instant) -> Shell {
        let mut shell = Shell::default();
        shell.maintain_geometry(VIEWPORT, bell());
        assert!(shell.mouse_down(VIEWPORT, bell(), 810.0, 10.0, now).0);
        shell.maintain_geometry(VIEWPORT, bell());
        shell.advance(now + Duration::from_millis(100));
        shell
    }

    pub(crate) fn item(id: &str, state: GlobalItemState, at: i64) -> GlobalItem {
        GlobalItem {
            target: format!("block:{id}"),
            title: id.to_owned(),
            source_location: "Acme / Notes".to_owned(),
            reminder_id: id.to_owned(),
            revision: format!("revision-{id}"),
            occurrence: Some(format!("occurrence-{id}")),
            sort_at_ms: Some(at),
            draft: koru_editor::reminders::Draft::new(ReminderDraft::Zoned(
                koru_core::time::ZonedDateTime::new(at, koru_core::time::TimeZoneName::utc()),
            )),
            snoozed: state == GlobalItemState::Snoozed,
            rule_paused: state == GlobalItemState::Paused,
            state,
        }
    }

    fn presentation_environment(tag: &str, zone: &str) -> PresentationEnvironment {
        PresentationEnvironment::new(
            LocaleFormat::for_system(Some(tag), None),
            IanaZoneRules::parse(zone).unwrap(),
        )
    }

    #[test]
    fn presentation_rebuilds_for_full_locale_and_device_zone() {
        let at = NaiveDate::from_ymd_opt(2026, 9, 3)
            .unwrap()
            .and_hms_opt(13, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let global = GlobalManagement::new(vec![item("zoned", GlobalItemState::ComingUp, at)]);
        let mut shell = Shell::default();
        let ny_h12 = PresentationEnvironment::new(
            LocaleFormat::for_system(Some("en-US"), Some(koru_i18n::time::HourCycle::H12)),
            IanaZoneRules::parse("America/New_York").unwrap(),
        );
        assert!(shell.set_global(&global, ny_h12));
        let h12 = shell.presentation.schedule_labels[0].clone();
        assert!(h12.contains("9:00 AM") && h12.contains("America/New_York"));

        let ny_h24 = PresentationEnvironment::new(
            LocaleFormat::for_system(Some("en-US"), Some(koru_i18n::time::HourCycle::H24)),
            IanaZoneRules::parse("America/New_York").unwrap(),
        );
        assert!(shell.set_global(&global, ny_h24));
        let h24 = shell.presentation.schedule_labels[0].clone();
        assert!(h24.contains("09:00") && h24.contains("America/New_York"));
        assert_ne!(h12, h24);

        assert!(shell.set_global(
            &global,
            presentation_environment("en-US", "America/Los_Angeles")
        ));
        let travelled = &shell.presentation.schedule_labels[0];
        assert!(travelled.contains("6:00 AM") && travelled.contains("America/Los_Angeles"));
    }

    #[test]
    fn device_zone_change_rebuilds_mixed_floating_and_zoned_order() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let mut floating = item("floating", GlobalItemState::ComingUp, 0);
        floating.sort_at_ms = None;
        floating.draft = Draft::new(ReminderDraft::Floating(FloatingDateTime::new(
            date.and_hms_opt(9, 0, 0).unwrap(),
        )));
        let mut zoned = item(
            "zoned",
            GlobalItemState::ComingUp,
            date.and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
        );
        zoned.sort_at_ms = None;
        let global = GlobalManagement::new(vec![floating, zoned]);
        let mut shell = Shell::default();
        shell.set_global(&global, presentation_environment("en-US", "UTC"));
        assert_eq!(shell.ordered_indices(GlobalFilter::ComingUp), &[0, 1]);
        assert!(shell.set_global(
            &global,
            presentation_environment("en-US", "America/Los_Angeles")
        ));
        assert_eq!(shell.ordered_indices(GlobalFilter::ComingUp), &[1, 0]);
    }

    #[test]
    fn badge_has_no_zero_and_caps_without_losing_semantic_state() {
        let mut shell = Shell::default();
        assert_eq!(shell.bell_label(Lang::En), "");
        let mut attention = AttentionData {
            fired: (0..99)
                .map(|_| FiredRow {
                    title: String::new(),
                    context: String::new(),
                    fired_at: String::new(),
                })
                .collect(),
            next: None,
        };
        shell.set_attention(attention.clone());
        assert_eq!(shell.bell_label(Lang::En), "99");
        attention.fired.push(FiredRow {
            title: String::new(),
            context: String::new(),
            fired_at: String::new(),
        });
        shell.set_attention(attention);
        assert_eq!(shell.bell_label(Lang::En), "99+");
        assert_eq!(shell.attention.fired.len(), 100);
    }

    #[test]
    fn handled_rows_leave_the_open_popover_and_badge_in_place() {
        let now = Instant::now();
        let rows = ["row 0", "row 1", "row 2"]
            .into_iter()
            .enumerate()
            .map(|(index, title)| item(title, GlobalItemState::ComingUp, index as i64 + 1))
            .collect::<Vec<_>>();
        let mut global = GlobalManagement::new(rows.clone());
        global.set_needs_attention(rows.iter().map(|row| GlobalAttention {
            reminder_id: row.reminder_id.clone(),
            occurrence: row.occurrence.clone().unwrap(),
            fired_at_ms: row.sort_at_ms.unwrap(),
        }));
        let mut shell = Shell::default();
        shell.set_global(&global, Lang::En);
        shell.mouse_down(VIEWPORT, bell(), 810.0, 10.0, now);
        assert!(shell.popover_live());
        assert_eq!(shell.bell_label(Lang::En), "3");

        let handled = rows[1].clone();
        shell.apply(Intent::Handled(handled.clone()), now);
        assert!(
            shell.popover_live(),
            "the action does not dismiss its surface"
        );

        let remaining = [rows[0].clone(), rows[2].clone()];
        let mut reconciled = GlobalManagement::new(remaining.to_vec());
        reconciled.set_needs_attention(remaining.iter().map(|row| GlobalAttention {
            reminder_id: row.reminder_id.clone(),
            occurrence: row.occurrence.clone().unwrap(),
            fired_at_ms: row.sort_at_ms.unwrap(),
        }));
        assert!(shell.set_global(&reconciled, Lang::En));
        assert!(shell.popover_live());
        assert_eq!(shell.bell_label(Lang::En), "2");
        assert_eq!(shell.attention.fired.len(), 2);
        assert!(
            shell
                .attention
                .fired
                .iter()
                .all(|row| row.title != handled.title)
        );
    }

    #[test]
    fn closing_popover_is_deaf_before_fade_finishes() {
        let now = Instant::now();
        let mut shell = opened_popover(now);
        let geometry = shell.geometry(VIEWPORT, bell());
        let inside = geometry.all.unwrap();
        let _ = shell.mouse_down(
            VIEWPORT,
            bell(),
            10.0,
            200.0,
            now + Duration::from_millis(101),
        );
        assert!(!shell.popover_live());
        assert!(shell.popover.is_some(), "still visible while fading");
        assert_eq!(
            shell
                .geometry(VIEWPORT, bell())
                .hit(inside.x + 1.0, inside.y + 1.0),
            Some(Hit::AllReminders)
        );
        assert!(
            !shell
                .mouse_down(
                    VIEWPORT,
                    bell(),
                    inside.x + 1.0,
                    inside.y + 1.0,
                    now + Duration::from_millis(102)
                )
                .0,
            "deaf surface does not claim input"
        );
    }

    #[test]
    fn outside_context_press_dismisses_popover_and_is_blocked() {
        let now = Instant::now();
        let mut shell = opened_popover(now);
        assert!(shell.context_press(
            VIEWPORT,
            bell(),
            20.0,
            200.0,
            now + Duration::from_millis(101),
        ));
        assert!(!shell.popover_live());
        assert!(shell.popover.is_some(), "the closing fade is still drawn");
        assert!(
            !shell.context_press(
                VIEWPORT,
                bell(),
                20.0,
                200.0,
                now + Duration::from_millis(102),
            ),
            "the fading popover is deaf"
        );
    }

    #[test]
    fn all_reminders_slides_inside_the_existing_bell_popover() {
        let now = Instant::now();
        let mut shell = opened_popover(now);
        let all = shell.geometry(VIEWPORT, bell()).all.unwrap();
        let (_, intent) = shell.mouse_down(
            VIEWPORT,
            bell(),
            all.x + 2.0,
            all.y + 2.0,
            now + Duration::from_millis(101),
        );
        assert_eq!(intent, Some(Intent::OpenAllReminders));
        shell.apply(intent.unwrap(), now + Duration::from_millis(101));
        assert!(shell.overlay_live());
        assert!(shell.page_motion.is_some());
        shell.maintain_geometry(VIEWPORT, bell());
        assert_eq!(
            shell.management.geometry.rect,
            shell.geometry.popover.map(|popover| popover.rect)
        );
        assert_eq!(shell.document_inset(), 0.0);
    }

    #[test]
    fn bell_popover_height_never_exceeds_four_hundred_pixels() {
        let now = Instant::now();
        let mut shell = Shell::default();
        shell.set_attention(AttentionData {
            fired: vec![
                FiredRow {
                    title: String::new(),
                    context: String::new(),
                    fired_at: String::new(),
                };
                10
            ],
            next: None,
        });
        shell.maintain_geometry(VIEWPORT, bell());
        shell.mouse_down(VIEWPORT, bell(), 810.0, 10.0, now);
        assert!(shell.geometry(VIEWPORT, bell()).popover.unwrap().rect.h <= POPOVER_MAX_H);
        shell.apply(Intent::OpenAllReminders, now);
        assert!(shell.geometry(VIEWPORT, bell()).popover.unwrap().rect.h <= POPOVER_MAX_H);
    }

    #[test]
    fn switching_pages_is_total_before_the_next_geometry_maintenance() {
        let now = Instant::now();
        let mut shell = opened_popover(now);
        shell.apply(Intent::OpenAllReminders, now);
        assert!(shell.pointer_move(VIEWPORT, bell(), 20.0, 200.0) || shell.hover.is_none());
        let mut scene = Scene::new();
        shell.draw_surfaces(
            &mut scene,
            Affine::IDENTITY,
            &mut TextEngine::new(),
            VIEWPORT,
            bell(),
            Lang::En,
        );
        shell.maintain_geometry(VIEWPORT, bell());
        assert!(shell.management.geometry.rect.is_some());
    }

    #[test]
    fn outside_press_closes_overlay_and_is_blocked() {
        let now = Instant::now();
        let mut shell = Shell::default();
        shell.apply(Intent::OpenAllReminders, now);
        shell.advance(now + Duration::from_millis(200));
        let (handled, _) = shell.mouse_down(
            VIEWPORT,
            bell(),
            20.0,
            200.0,
            now + Duration::from_millis(201),
        );
        assert!(handled);
        assert!(shell.popover.is_some());
        assert!(shell.overlay_visible(), "the close slide is still drawn");
        assert!(!shell.overlay_live());
        assert!(!shell.blocks_pointer(VIEWPORT, bell(), 20.0, 200.0));
        assert!(
            !shell
                .mouse_down(
                    VIEWPORT,
                    bell(),
                    20.0,
                    200.0,
                    now + Duration::from_millis(202),
                )
                .0,
            "the closing overlay is immediately deaf"
        );
        assert!(
            !shell
                .key(
                    VIEWPORT,
                    bell(),
                    &KeyboardKey::Named(NamedKey::F9),
                    now + Duration::from_millis(202)
                )
                .0,
            "keys pass through the closing slide"
        );
    }

    #[test]
    fn small_viewport_keeps_surfaces_inside_edges() {
        let now = Instant::now();
        let mut shell = Shell::default();
        shell.maintain_geometry((180.0, 220.0), Rect::new(130.0, 0.0, 36.0, 36.0));
        shell.mouse_down(
            (180.0, 220.0),
            Rect::new(130.0, 0.0, 36.0, 36.0),
            140.0,
            10.0,
            now,
        );
        let popover = shell
            .geometry((180.0, 220.0), Rect::new(130.0, 0.0, 36.0, 36.0))
            .popover
            .unwrap()
            .rect;
        assert!(popover.x >= 8.0 && popover.x + popover.w <= 172.0);
        shell.apply(Intent::OpenAllReminders, now);
        shell.advance(now + Duration::from_millis(200));
        shell.geometry((180.0, 220.0), bell());
        let overlay = shell.management.geometry.rect.unwrap();
        assert!(overlay.x >= 8.0);
        assert!(overlay.x + overlay.w <= 172.01);
    }

    #[test]
    fn profile_reset_closes_and_clears_transient_state() {
        let now = Instant::now();
        let mut shell = opened_popover(now);
        shell.apply(Intent::OpenAllReminders, now);
        shell.bell_pressed = true;
        shell.reset_profile();
        assert!(!shell.overlay_visible());
        assert!(shell.popover.is_none());
        assert!(!shell.bell_pressed);
        assert_eq!(shell.management.filter, Filter::All);
    }

    #[test]
    fn bell_and_quiet_popover_emit_visual_commands() {
        let now = Instant::now();
        let shell = opened_popover(now);
        let mut scene = Scene::new();
        let mut engine = TextEngine::new();
        shell.draw_bell(
            &mut scene,
            Affine::IDENTITY,
            &mut engine,
            bell(),
            true,
            Lang::En,
        );
        shell.draw_surfaces(
            &mut scene,
            Affine::IDENTITY,
            &mut engine,
            VIEWPORT,
            bell(),
            Lang::En,
        );
        assert!(!scene.encoding().is_empty());
    }

    #[test]
    fn next_stays_above_recent_fired_rows_and_all_reminders_stays_at_the_top() {
        let now = Instant::now();
        let mut shell = Shell::default();
        shell.set_attention(AttentionData {
            fired: vec![FiredRow {
                title: "Pay rent".into(),
                context: "Home · Finance".into(),
                fired_at: "2 min ago".into(),
            }],
            next: Some("Tomorrow at 9:00 AM".into()),
        });
        shell.maintain_geometry(VIEWPORT, bell());
        shell.mouse_down(VIEWPORT, bell(), 810.0, 10.0, now);
        let geometry = shell.geometry(VIEWPORT, bell());
        assert_eq!(geometry.fired.len(), 1);
        let all = geometry.all.expect("top-level All reminders action");
        let next = geometry.quiet.expect("Next remains visible with activity");
        let row = geometry.fired[0];
        assert!(all.y < next.y);
        assert!(next.y < row.y);
        assert_eq!(geometry.hit(row.x + 1.0, row.y + 1.0), Some(Hit::Fired(0)));
    }

    #[test]
    fn next_and_coming_up_are_chronological_with_identity_ties() {
        let mut shell = Shell::default();
        shell.set_global(
            &GlobalManagement::new(vec![
                item("late", GlobalItemState::ComingUp, 2_000),
                item("tie-b", GlobalItemState::ComingUp, 1_000),
                item("tie-a", GlobalItemState::ComingUp, 1_000),
            ]),
            Lang::En,
        );
        let ordered = shell.ordered_indices(GlobalFilter::ComingUp);
        assert_eq!(
            ordered
                .iter()
                .map(|index| shell.global.rows[*index].reminder_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tie-a", "tie-b", "late"]
        );
        assert_eq!(
            shell.attention.next,
            Some(schedule_label(
                &shell.presentation.locale,
                &shell.presentation.display_rules,
                &shell.global.rows[2],
            ))
        );
    }

    #[test]
    fn an_earlier_snooze_is_the_compact_popovers_next_delivery() {
        let snoozed = item("snoozed", GlobalItemState::Snoozed, 1_000);
        let scheduled = item("scheduled", GlobalItemState::ComingUp, 2_000);
        let mut shell = Shell::default();
        shell.set_global(
            &GlobalManagement::new(vec![scheduled, snoozed.clone()]),
            Lang::En,
        );

        assert_eq!(
            shell.attention.next,
            Some(format_instant(
                &shell.presentation.locale,
                &shell.presentation.display_rules,
                1_000,
            ))
        );
        assert_eq!(
            schedule_label(
                &shell.presentation.locale,
                &shell.presentation.display_rules,
                &snoozed,
            ),
            format_instant(
                &shell.presentation.locale,
                &shell.presentation.display_rules,
                1_000,
            ),
            "a snoozed row names its next delivery, not its authored schedule"
        );
    }

    #[test]
    fn bell_accepts_keyboard_focus_and_activation() {
        let now = Instant::now();
        let mut shell = Shell::default();
        assert!(shell.focus_bell());
        assert!(shell.bell_focused);
        let (handled, intent) =
            shell.key(VIEWPORT, bell(), &KeyboardKey::Named(NamedKey::Enter), now);
        assert!(handled);
        assert!(intent.is_none());
        assert!(shell.popover_live());
    }

    #[test]
    fn snooze_amounts_and_accumulated_duration_use_locale_numbers() {
        for lang in Lang::ALL {
            let amount = koru_i18n::number::decimal(lang, 15);
            let count = koru_i18n::number::decimal(lang, 3);
            let increment = snooze_increment_label(lang, SnoozeIncrement::FifteenMinutes, 3);
            assert!(increment.contains(&amount), "{}", lang.tag());
            assert!(increment.contains(&count), "{}", lang.tag());
            let total = snooze_duration_label(lang, 165);
            assert!(total.contains(&koru_i18n::number::decimal(lang, 2)));
            assert!(total.contains(&koru_i18n::number::decimal(lang, 45)));
        }
    }

    #[test]
    fn current_bell_hit_works_before_maintenance_and_after_relayout_without_rebuild() {
        let mut cold = Shell::default();
        let current = bell();
        let (x, y) = current.centre();
        assert!(cold.pointer_move(VIEWPORT, current, x, y));

        let mut shell = Shell::default();
        shell.maintain_geometry(VIEWPORT, current);
        let fired = shell.geometry.fired.as_ptr();
        let focus = shell.geometry.focus_order.as_ptr();
        let moved = Rect::new(700.0, 0.0, 36.0, 36.0);
        shell.pointer_move(VIEWPORT, moved, 600.0, 10.0);
        let (moved_x, moved_y) = moved.centre();
        assert!(shell.pointer_move(VIEWPORT, moved, moved_x, moved_y));
        assert_eq!(shell.geometry.fired.as_ptr(), fired);
        assert_eq!(shell.geometry.focus_order.as_ptr(), focus);
    }

    #[test]
    fn action_metrics_invalidate_for_font_role_changes() {
        let mut shell = Shell::default();
        let mut engine = TextEngine::new();
        assert!(shell.prepare_action_widths(&mut engine, Lang::En));
        assert!(!shell.prepare_action_widths(&mut engine, Lang::En));
        let current = ActionMetricKey::current(Lang::En);
        let other_font = theme::FontChoice::ALL
            .iter()
            .copied()
            .find(|font| *font != current.font)
            .unwrap();
        shell.presentation.action_metric_key = Some(ActionMetricKey {
            font: other_font,
            ..current
        });
        assert!(shell.prepare_action_widths(&mut engine, Lang::En));
        shell.invalidate_metrics();
        assert!(shell.presentation.action_metric_key.is_none());
        assert!(shell.prepare_action_widths(&mut engine, Lang::En));
    }

    #[test]
    fn localized_fired_actions_use_measured_hierarchy_without_overlap() {
        let now = Instant::now();
        for lang in Lang::ALL {
            let mut shell = Shell::default();
            shell.set_global(
                &GlobalManagement::new(vec![item(
                    "fired",
                    GlobalItemState::NeedsAttention,
                    1_700_000_000_000,
                )]),
                lang,
            );
            shell.prepare_action_widths(&mut TextEngine::new(), lang);
            shell.maintain_geometry(VIEWPORT, bell());
            shell.mouse_down(VIEWPORT, bell(), 810.0, 10.0, now);
            let geometry = shell.geometry(VIEWPORT, bell());
            let row = geometry.fired[0];
            let actions = geometry.fired_actions[0];
            for (rect, action) in
                actions
                    .into_iter()
                    .zip([RowAction::Open, RowAction::Snooze, RowAction::Handled])
            {
                assert_eq!(
                    rect.w,
                    shell.presentation.action_widths[action.width_slot(false)]
                );
                assert!(rect.x >= row.x && rect.x + rect.w <= row.x + row.w);
            }
            assert!(actions[0].x + actions[0].w <= actions[1].x);
            assert!(actions[1].x + actions[1].w <= actions[2].x);
        }
    }
}
