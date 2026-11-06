// SPDX-License-Identifier: MPL-2.0

//! The editor's edge onto [`koru_when`] — the one place an unresolved grammar
//! answer becomes a model type or a reminder draft.
//!
//! **The grammar itself is not here and must not come back.** Its accepted
//! forms and refusals live with its vocabulary and corpus. This module owns the
//! mapping the leaf parser deliberately cannot make: legacy `When::Date` /
//! `When::Wall` compatibility for `/due`, plus separate due and reminder
//! resolution from an explicit [`ResolutionContext`].
//!
//! That split keeps `koru-when` independent of `koru-core`. The zone, clock,
//! locale order, settings, selection, and optional due anchor all arrive as
//! values; resolution performs no I/O or process-global lookup ([ADR-183]
//! rule 4, [ADR-191] as amended 2026-09-02).
//!
//! **[`to_temporal`] is the one place a wall clock becomes a stored deadline**,
//! and the only thing here that reads the machine — the system zone, through
//! [`crate::utils::zone::SystemZone`]. It exists so every surface that turns
//! typed words into a deadline stores the same value: the editor's `/due`, the
//! Tasks page's picker and its capture row all call it rather than each
//! resolving a zone of its own ([ADR-206] rule 4).
//!
//! [ADR-206]: ../../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
//! [ADR-214]: ../../../../docs/decisions/ADR-201-225.md#adr-214-whenparser--natural-language-dates-are-their-own-crate-with-two-reference-points-and-a-slash-order-that-is-a-parameter

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike};
use koru_core::time::{
    DateOnly, FloatingDateTime, Temporal, TimeZoneName, ZoneRules, ZonedDateTime,
};
use koru_time_zone::{IanaZoneRules, ZoneError};
use koru_when::{DateExpression, Daypart, DueExpression, TemporalExpression, TimeExpression, When};

pub use koru_when::{
    DEFAULT_DATE_ORDER, DateOrder, DueRelation, DurationExpression, DurationOrigin, DurationSign,
    DurationUnit, Language, Reference,
};

/// A device or profile choice used only while resolving week-relative
/// reminder phrases. It is explicit context, never parser-global state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeekStart {
    #[default]
    Monday,
    Saturday,
    Sunday,
}

/// Every input needed to resolve an expression. Resolution never reads a
/// clock, environment variable, setting, document, Store, or process global.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionContext {
    pub now_ms: i64,
    pub language: Language,
    pub date_order: DateOrder,
    pub current_zone: TimeZoneName,
    pub selection: Option<FloatingDateTime>,
    pub end_of_day: Option<NaiveTime>,
    pub due_anchor: Option<Temporal>,
    pub system_week_start: WeekStart,
    pub week_start_override: Option<WeekStart>,
}

/// Stable semantic outcomes. A surface localizes these codes; this module
/// never returns user-visible prose.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResolutionIssue {
    AmbiguousZone,
    UnknownZone,
    MissingEndOfDay,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution<T> {
    Resolved(T),
    Passed { requested: T, proposal: T },
    Needs(ResolutionIssue),
}

/// A reminder draft has either a floating wall, one canonical zoned instant,
/// or phase-one at/before-due intent. It is not a persisted Store type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReminderDraft {
    Floating(FloatingDateTime),
    Zoned(ZonedDateTime),
    Relative {
        relation: koru_when::DueRelation,
        offset: Option<DurationExpression>,
        anchor: Option<Temporal>,
        preview: Option<ReminderMoment>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReminderMoment {
    Floating(FloatingDateTime),
    Zoned(ZonedDateTime),
}

/// What a phrase resolved to, in the editor's own vocabulary.
///
/// Field-for-field [`koru_when::When`], with the two arms carrying the model's
/// types instead of chrono's. The name is the one the call sites already used,
/// which is why the swap moved no code outside this file.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Parsed {
    /// A day: `friday`, `aug 31`, `in 3d`, `10/28`.
    Date(DateOnly),
    /// A wall-clock reading: `tomorrow 9am`, `in 90m`, `+4h`. Floating — it
    /// says nothing about where the person is, and the commit is what decides.
    Floating(FloatingDateTime),
}

/// Parse `input` against `at`. `None` means the grammar does not know the
/// phrase — the caller opens a picker rather than guessing.
///
/// `order` is passed by the caller rather than defaulted here because locale
/// decides whether numeric fields are month-first, day-first, or year-first.
pub fn parse(input: &str, at: Reference, order: DateOrder) -> Option<Parsed> {
    Some(match koru_when::parse(input, at, order)? {
        When::Date(d) => Parsed::Date(DateOnly::new(d)),
        When::Wall(t) => Parsed::Floating(FloatingDateTime::new(t)),
    })
}

/// **What a parsed phrase stores.** A day stays a calendar date; a wall clock
/// crosses the zone seam and becomes the shared instant collaborators can
/// agree on ([ADR-206]).
///
/// One function, every caller — the editor's `/due`, the Tasks page's picker,
/// the capture row's date word — because two surfaces that each resolved a
/// zone of their own would store two deadlines for one phrase and both would
/// look right. The zone is the resolution seam's answer: the workspace's
/// declared default when it has one, this machine's otherwise.
///
/// [ADR-206]: ../../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
pub fn to_temporal(parsed: Parsed) -> Temporal {
    match parsed {
        Parsed::Date(d) => Temporal::Date(d),
        Parsed::Floating(wall) => {
            Temporal::Zoned(crate::utils::zone::SystemZone::detect().resolve_for_shared(None, wall))
        }
    }
}

/// [`to_temporal`] with the zone in hand — what the test drives, so the
/// assertion is about the mapping rather than about the machine it runs on.
#[cfg(test)]
pub(crate) fn to_temporal_in(parsed: Parsed, zone: &crate::utils::zone::SystemZone) -> Temporal {
    match parsed {
        Parsed::Date(d) => Temporal::Date(d),
        Parsed::Floating(wall) => Temporal::Zoned(zone.resolve_for_shared(None, wall)),
    }
}

/// Parse with the context's language and resolve for a shared due value.
pub fn parse_due(input: &str, context: &ResolutionContext) -> Resolution<Temporal> {
    let Some(expression) = koru_when::parse_expression(input, context.language, context.date_order)
    else {
        return Resolution::Needs(ResolutionIssue::Unsupported);
    };
    resolve_due(&expression, context)
}

/// Parse with the context's language and resolve for a reminder draft.
pub fn parse_reminder(input: &str, context: &ResolutionContext) -> Resolution<ReminderDraft> {
    let Some(expression) =
        koru_when::parse_reminder_expression(input, context.language, context.date_order)
    else {
        return Resolution::Needs(ResolutionIssue::Unsupported);
    };
    resolve_reminder(&expression, context)
}

/// Shared due mapping. Timed values resolve in the explicit IANA zone or the
/// injected current zone; date-only values remain date-only.
pub fn resolve_due(
    expression: &TemporalExpression,
    context: &ResolutionContext,
) -> Resolution<Temporal> {
    let result = match expression {
        TemporalExpression::Date(date) => {
            resolve_date(*date, context, None).map(|date| Temporal::Date(DateOnly::new(date)))
        }
        TemporalExpression::Wall { date, time, zone } => {
            resolve_wall(*date, *time, zone.as_deref(), context, true)
                .map(|(_, wall, rules)| Temporal::Zoned(wall.in_zone(&rules)))
        }
        TemporalExpression::Duration(duration) => {
            match resolve_duration(*duration, context, None) {
                Ok(LegacyResolved::Date(date)) => Ok(Temporal::Date(DateOnly::new(date))),
                Ok(LegacyResolved::Wall(wall)) => current_rules(context)
                    .map(|rules| Temporal::Zoned(FloatingDateTime::new(wall).in_zone(&rules))),
                Err(issue) => Err(issue),
            }
        }
        TemporalExpression::Due(_) => Err(ResolutionIssue::Unsupported),
    };
    match result {
        Ok(value) => Resolution::Resolved(value),
        Err(issue) => Resolution::Needs(issue),
    }
}

/// Personal reminder mapping. Date-only drafts become visible floating 09:00;
/// no-zone walls stay floating, and explicit zones return one canonical IANA
/// name plus one instant. Past input returns a proposal instead of committing.
pub fn resolve_reminder(
    expression: &TemporalExpression,
    context: &ResolutionContext,
) -> Resolution<ReminderDraft> {
    if let TemporalExpression::Due(due) = expression {
        return resolve_relative(*due, context);
    }

    let result: Result<ReminderDraft, ResolutionIssue> = (|| match expression {
        TemporalExpression::Date(date) => {
            let day = resolve_date(*date, context, None)?;
            Ok(ReminderDraft::Floating(
                DateOnly::new(day).at(default_reminder_time()),
            ))
        }
        TemporalExpression::Wall { date, time, zone } => {
            let (_, wall, rules) = resolve_wall(*date, *time, zone.as_deref(), context, false)?;
            if zone.is_some() {
                Ok(ReminderDraft::Zoned(wall.in_zone(&rules)))
            } else {
                Ok(ReminderDraft::Floating(wall))
            }
        }
        TemporalExpression::Duration(duration) => match resolve_duration(*duration, context, None)?
        {
            LegacyResolved::Date(date) => Ok(ReminderDraft::Floating(
                DateOnly::new(date).at(default_reminder_time()),
            )),
            LegacyResolved::Wall(wall) => Ok(ReminderDraft::Floating(FloatingDateTime::new(
                reminder_minute(wall),
            ))),
        },
        TemporalExpression::Due(_) => unreachable!("handled above"),
    })();

    let draft = match result {
        Ok(draft) => draft,
        Err(issue) => return Resolution::Needs(issue),
    };
    review_reminder_draft(draft, context)
}

/// Reminder surfaces expose minute precision, so a reminder must not retain a
/// hidden second that makes its displayed minute a false delivery promise.
/// Shared due parsing keeps its existing exact-duration semantics; this is the
/// reminder boundary only.
fn reminder_minute(wall: NaiveDateTime) -> NaiveDateTime {
    wall.with_second(0)
        .and_then(|wall| wall.with_nanosecond(0))
        .expect("zero is always a valid second and nanosecond")
}

/// Review an already-semantic reminder draft against the same passed-time
/// policy used after natural-language parsing.
///
/// History Recreate and existing-item handoffs already carry a typed draft;
/// reparsing display text would lose relative intent and canonical zones. This
/// pure seam keeps those callers on the ordinary proposal path without
/// consulting a clock or the Store.
pub fn review_reminder_draft(
    draft: ReminderDraft,
    context: &ResolutionContext,
) -> Resolution<ReminderDraft> {
    let draft = match draft {
        ReminderDraft::Relative {
            relation, offset, ..
        } => ReminderDraft::Relative {
            relation,
            offset,
            anchor: context.due_anchor.clone(),
            preview: context
                .due_anchor
                .as_ref()
                .and_then(|anchor| relative_preview(anchor, relation, offset).ok()),
        },
        draft => draft,
    };
    if is_past(&draft, context) {
        match move_draft_to_future(draft.clone(), context) {
            Ok(proposal) => Resolution::Passed {
                requested: draft,
                proposal,
            },
            Err(issue) => Resolution::Needs(issue),
        }
    } else {
        Resolution::Resolved(draft)
    }
}

fn resolve_relative(due: DueExpression, context: &ResolutionContext) -> Resolution<ReminderDraft> {
    let (relation, offset) = match due {
        DueExpression::At => (koru_when::DueRelation::At, None),
        DueExpression::Before(offset)
            if offset.unit != DurationUnit::Month
                && offset.amount != 0
                && offset.origin == DurationOrigin::Now
                && offset.sign == DurationSign::Forward =>
        {
            (koru_when::DueRelation::Before, Some(offset))
        }
        DueExpression::Before(_) => return Resolution::Needs(ResolutionIssue::Unsupported),
    };
    let preview = context
        .due_anchor
        .as_ref()
        .and_then(|anchor| relative_preview(anchor, relation, offset).ok());
    Resolution::Resolved(ReminderDraft::Relative {
        relation,
        offset,
        anchor: context.due_anchor.clone(),
        preview,
    })
}

fn relative_preview(
    anchor: &Temporal,
    relation: koru_when::DueRelation,
    offset: Option<DurationExpression>,
) -> Result<ReminderMoment, ResolutionIssue> {
    let base = match anchor {
        Temporal::Date(date) => ReminderMoment::Floating(date.at(default_reminder_time())),
        Temporal::Floating(wall) => ReminderMoment::Floating(*wall),
        Temporal::Zoned(zoned) => ReminderMoment::Zoned(zoned.clone()),
    };
    if relation == koru_when::DueRelation::At {
        return Ok(base);
    }
    let offset = offset.ok_or(ResolutionIssue::Unsupported)?;
    let amount = i64::from(offset.amount);
    match (base, offset.unit) {
        (ReminderMoment::Floating(wall), DurationUnit::Minute) => {
            checked_wall_sub(wall, TimeDelta::minutes(amount)).map(ReminderMoment::Floating)
        }
        (ReminderMoment::Floating(wall), DurationUnit::Hour) => {
            checked_wall_sub(wall, TimeDelta::hours(amount)).map(ReminderMoment::Floating)
        }
        (ReminderMoment::Floating(wall), DurationUnit::Day) => {
            checked_wall_sub(wall, TimeDelta::days(amount)).map(ReminderMoment::Floating)
        }
        (ReminderMoment::Floating(wall), DurationUnit::Week) => {
            checked_wall_sub(wall, TimeDelta::weeks(amount)).map(ReminderMoment::Floating)
        }
        (ReminderMoment::Zoned(zoned), DurationUnit::Minute) => {
            Ok(ReminderMoment::Zoned(ZonedDateTime::new(
                zoned
                    .instant_ms()
                    .checked_sub(TimeDelta::minutes(amount).num_milliseconds())
                    .ok_or(ResolutionIssue::Unsupported)?,
                zoned.zone().clone(),
            )))
        }
        (ReminderMoment::Zoned(zoned), DurationUnit::Hour) => {
            Ok(ReminderMoment::Zoned(ZonedDateTime::new(
                zoned
                    .instant_ms()
                    .checked_sub(TimeDelta::hours(amount).num_milliseconds())
                    .ok_or(ResolutionIssue::Unsupported)?,
                zoned.zone().clone(),
            )))
        }
        (ReminderMoment::Zoned(zoned), DurationUnit::Day | DurationUnit::Week) => {
            let rules = IanaZoneRules::from_name(zoned.zone()).map_err(zone_issue)?;
            let days = if offset.unit == DurationUnit::Week {
                amount.checked_mul(7).ok_or(ResolutionIssue::Unsupported)?
            } else {
                amount
            };
            let wall = zoned
                .wall_in(&rules)
                .wall()
                .checked_sub_signed(TimeDelta::days(days))
                .ok_or(ResolutionIssue::Unsupported)?;
            Ok(ReminderMoment::Zoned(
                FloatingDateTime::new(wall).in_zone(&rules),
            ))
        }
        (_, DurationUnit::Month) => Err(ResolutionIssue::Unsupported),
    }
}

#[derive(Copy, Clone)]
enum LegacyResolved {
    Date(NaiveDate),
    Wall(NaiveDateTime),
}

fn resolve_duration(
    duration: DurationExpression,
    context: &ResolutionContext,
    rules: Option<&IanaZoneRules>,
) -> Result<LegacyResolved, ResolutionIssue> {
    if duration.amount == 0
        || (duration.origin == DurationOrigin::Now && duration.sign == DurationSign::Backward)
    {
        return Err(ResolutionIssue::Unsupported);
    }
    let owned;
    let rules = match rules {
        Some(rules) => rules,
        None => {
            owned = current_rules(context)?;
            &owned
        }
    };
    let now = rules.wall_of(context.now_ms);
    let base = match duration.origin {
        DurationOrigin::Now => now,
        DurationOrigin::Selection => context
            .selection
            .map(FloatingDateTime::wall)
            .ok_or(ResolutionIssue::Unsupported)?,
    };
    let sign = if duration.sign == DurationSign::Backward {
        -1
    } else {
        1
    };
    let amount = i64::from(duration.amount) * sign;
    match duration.unit {
        DurationUnit::Minute => base
            .checked_add_signed(TimeDelta::minutes(amount))
            .map(LegacyResolved::Wall),
        DurationUnit::Hour => base
            .checked_add_signed(TimeDelta::hours(amount))
            .map(LegacyResolved::Wall),
        DurationUnit::Day => base
            .date()
            .checked_add_signed(TimeDelta::days(amount))
            .map(LegacyResolved::Date),
        DurationUnit::Week => base
            .date()
            .checked_add_signed(TimeDelta::weeks(amount))
            .map(LegacyResolved::Date),
        DurationUnit::Month => shift_month(base.date(), amount).map(LegacyResolved::Date),
    }
    .ok_or(ResolutionIssue::Unsupported)
}

fn resolve_wall(
    date: Option<DateExpression>,
    time: TimeExpression,
    zone: Option<&str>,
    context: &ResolutionContext,
    legacy_bare_time_roll: bool,
) -> Result<(bool, FloatingDateTime, IanaZoneRules), ResolutionIssue> {
    let rules = match zone {
        Some(name) => IanaZoneRules::parse(name).map_err(zone_issue)?,
        None => current_rules(context)?,
    };
    let now = rules.wall_of(context.now_ms);
    let mut day = match date {
        Some(date) => resolve_date(date, context, Some(&rules))?,
        None => now.date(),
    };
    let clock = match time {
        TimeExpression::Clock(time) => time,
        TimeExpression::Daypart(Daypart::Morning) => NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        TimeExpression::Daypart(Daypart::Afternoon) => NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
        TimeExpression::Daypart(Daypart::Evening | Daypart::Tonight) => {
            NaiveTime::from_hms_opt(19, 0, 0).unwrap()
        }
        TimeExpression::EndOfDay => context.end_of_day.ok_or(ResolutionIssue::MissingEndOfDay)?,
    };
    let mut wall = day.and_time(clock);
    let tonight = time == TimeExpression::Daypart(Daypart::Tonight);
    if date.is_none() && (tonight || legacy_bare_time_roll) && wall <= now {
        day = day
            .checked_add_signed(TimeDelta::days(1))
            .ok_or(ResolutionIssue::Unsupported)?;
        wall = day.and_time(clock);
    }
    Ok((zone.is_some(), FloatingDateTime::new(wall), rules))
}

fn resolve_date(
    expression: DateExpression,
    context: &ResolutionContext,
    rules: Option<&IanaZoneRules>,
) -> Result<NaiveDate, ResolutionIssue> {
    let owned;
    let rules = match rules {
        Some(rules) => rules,
        None => {
            owned = current_rules(context)?;
            &owned
        }
    };
    let today = rules.wall_of(context.now_ms).date();
    match expression {
        DateExpression::Today => Ok(today),
        DateExpression::Tomorrow => today
            .checked_add_signed(TimeDelta::days(1))
            .ok_or(ResolutionIssue::Unsupported),
        DateExpression::Yesterday => today
            .checked_sub_signed(TimeDelta::days(1))
            .ok_or(ResolutionIssue::Unsupported),
        DateExpression::NextWeek => {
            let starts_on = context
                .week_start_override
                .unwrap_or(context.system_week_start);
            let start = match starts_on {
                WeekStart::Monday => 0,
                WeekStart::Saturday => 5,
                WeekStart::Sunday => 6,
            };
            let current = today.weekday().num_days_from_monday();
            let days_into_week = (current + 7 - start) % 7;
            let delta = 7 - days_into_week;
            today
                .checked_add_signed(TimeDelta::days(i64::from(delta)))
                .ok_or(ResolutionIssue::Unsupported)
        }
        // That weekday in next week: the week's first day, by the person's
        // week start exactly as `NextWeek` reads it, then on to the weekday.
        DateExpression::NextWeekday(index) => {
            if index > 6 {
                return Err(ResolutionIssue::Unsupported);
            }
            let start = match context
                .week_start_override
                .unwrap_or(context.system_week_start)
            {
                WeekStart::Monday => 0,
                WeekStart::Saturday => 5,
                WeekStart::Sunday => 6,
            };
            let current = today.weekday().num_days_from_monday();
            let to_next_week = 7 - (current + 7 - start) % 7;
            let into_week = (u32::from(index) + 7 - start) % 7;
            today
                .checked_add_signed(TimeDelta::days(i64::from(to_next_week + into_week)))
                .ok_or(ResolutionIssue::Unsupported)
        }
        DateExpression::Weekday(index) => {
            if index > 6 {
                return Err(ResolutionIssue::Unsupported);
            }
            let current = today.weekday().num_days_from_monday();
            let mut delta = (u32::from(index) + 7 - current) % 7;
            if delta == 0 {
                delta = 7;
            }
            today
                .checked_add_signed(TimeDelta::days(i64::from(delta)))
                .ok_or(ResolutionIssue::Unsupported)
        }
        DateExpression::MonthDay { month, day, year } => {
            if let Some(year) = year {
                return NaiveDate::from_ymd_opt(year, month, day)
                    .ok_or(ResolutionIssue::Unsupported);
            }
            let this = NaiveDate::from_ymd_opt(today.year(), month, day)
                .ok_or(ResolutionIssue::Unsupported)?;
            if this < today {
                let next_year = today
                    .year()
                    .checked_add(1)
                    .ok_or(ResolutionIssue::Unsupported)?;
                NaiveDate::from_ymd_opt(next_year, month, day).ok_or(ResolutionIssue::Unsupported)
            } else {
                Ok(this)
            }
        }
    }
}

fn current_rules(context: &ResolutionContext) -> Result<IanaZoneRules, ResolutionIssue> {
    IanaZoneRules::from_name(&context.current_zone).map_err(zone_issue)
}

fn zone_issue(error: ZoneError) -> ResolutionIssue {
    match error {
        ZoneError::AmbiguousAbbreviation => ResolutionIssue::AmbiguousZone,
        ZoneError::UnknownZone => ResolutionIssue::UnknownZone,
        ZoneError::InvalidWall => ResolutionIssue::Unsupported,
    }
}

fn default_reminder_time() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 0, 0).unwrap()
}

fn is_past(draft: &ReminderDraft, context: &ResolutionContext) -> bool {
    match draft {
        ReminderDraft::Floating(wall) => current_rules(context)
            .map(|rules| wall.wall() <= rules.wall_of(context.now_ms))
            .unwrap_or(false),
        ReminderDraft::Zoned(zoned) => zoned.instant_ms() <= context.now_ms,
        ReminderDraft::Relative { .. } => false,
    }
}

fn move_draft_to_future(
    draft: ReminderDraft,
    context: &ResolutionContext,
) -> Result<ReminderDraft, ResolutionIssue> {
    match draft {
        ReminderDraft::Floating(wall) => {
            let rules = current_rules(context)?;
            let future = next_future_wall(wall.wall(), rules.wall_of(context.now_ms))?;
            Ok(ReminderDraft::Floating(FloatingDateTime::new(future)))
        }
        ReminderDraft::Zoned(zoned) => {
            let rules = IanaZoneRules::from_name(zoned.zone()).map_err(zone_issue)?;
            let future =
                next_future_wall(zoned.wall_in(&rules).wall(), rules.wall_of(context.now_ms))?;
            Ok(ReminderDraft::Zoned(
                FloatingDateTime::new(future).in_zone(&rules),
            ))
        }
        relative @ ReminderDraft::Relative { .. } => Ok(relative),
    }
}

fn next_future_wall(
    requested: NaiveDateTime,
    now: NaiveDateTime,
) -> Result<NaiveDateTime, ResolutionIssue> {
    let elapsed_days = now
        .date()
        .signed_duration_since(requested.date())
        .num_days();
    let same_clock_today = requested
        .checked_add_signed(TimeDelta::days(elapsed_days.max(0)))
        .ok_or(ResolutionIssue::Unsupported)?;
    if same_clock_today > now {
        Ok(same_clock_today)
    } else {
        same_clock_today
            .checked_add_signed(TimeDelta::days(1))
            .ok_or(ResolutionIssue::Unsupported)
    }
}

fn checked_wall_sub(
    wall: FloatingDateTime,
    delta: TimeDelta,
) -> Result<FloatingDateTime, ResolutionIssue> {
    wall.wall()
        .checked_sub_signed(delta)
        .map(FloatingDateTime::new)
        .ok_or(ResolutionIssue::Unsupported)
}

fn shift_month(date: NaiveDate, delta: i64) -> Option<NaiveDate> {
    let total = i64::from(date.year())
        .checked_mul(12)?
        .checked_add(i64::from(date.month0()))?
        .checked_add(delta)?;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month0 = u32::try_from(total.rem_euclid(12)).ok()?;
    let first = NaiveDate::from_ymd_opt(year, month0 + 1, 1)?;
    let next = if month0 == 11 {
        NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month0 + 2, 1)
    }?;
    let days = next.signed_duration_since(first).num_days() as u32;
    first.with_day(date.day().min(days))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime, Timelike};

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 8, 2)
            .unwrap()
            .and_hms_opt(10, 15, 0)
            .unwrap()
    }

    fn context(language: Language) -> ResolutionContext {
        ResolutionContext {
            now_ms: now().and_utc().timestamp_millis(),
            language,
            date_order: DateOrder::MonthFirst,
            current_zone: TimeZoneName::utc(),
            selection: None,
            end_of_day: None,
            due_anchor: None,
            system_week_start: WeekStart::Monday,
            week_start_override: None,
        }
    }

    /// The mapping, both arms — the only behaviour this module has. The
    /// grammar's own rules are `koru-when`'s to prove.
    #[test]
    fn each_arm_becomes_its_model_type() {
        let at = Reference::without_selection(now());
        assert_eq!(
            parse("aug 31", at, DEFAULT_DATE_ORDER),
            Some(Parsed::Date(DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
            ))),
            "a day arrives as a DateOnly"
        );
        assert_eq!(
            parse("tomorrow 9am", at, DEFAULT_DATE_ORDER),
            Some(Parsed::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap()
            ))),
            "a clock arrives as a FloatingDateTime"
        );
        assert_eq!(parse("asdf", at, DEFAULT_DATE_ORDER), None);
    }

    /// **The selection is threaded, not defaulted.** A signed duration counts
    /// from it, and with the two reference points apart the difference is
    /// visible — which is the only condition under which a wrong-reference bug
    /// can fail a test at all (ADR-214 §3).
    #[test]
    fn a_signed_duration_reads_the_selection_the_caller_supplied() {
        let far = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let at = Reference::on_day(now(), far);
        assert_eq!(
            parse("+3d", at, DEFAULT_DATE_ORDER),
            Some(Parsed::Date(DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()
            ))),
            "+3d counts from the selection"
        );
        assert_eq!(
            parse("3d", at, DEFAULT_DATE_ORDER),
            Some(Parsed::Date(DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()
            ))),
            "and an unsigned one still counts from the clock"
        );
    }

    // ------------------------------------------------------- to_temporal

    use crate::utils::zone::SystemZone;

    fn far() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()
    }

    /// A day stays a calendar date and a clock becomes an instant — the two
    /// shapes `koru.due` stores, and never a floating one.
    #[test]
    fn a_day_stores_a_date_and_a_clock_stores_a_zoned_instant() {
        let at = Reference::without_selection(now());
        let day = parse("aug 31", at, DEFAULT_DATE_ORDER).unwrap();
        assert_eq!(
            to_temporal_in(day, &SystemZone::utc()),
            Temporal::Date(DateOnly::new(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()))
        );
        let timed = parse("tomorrow 9am", at, DEFAULT_DATE_ORDER).unwrap();
        let Temporal::Zoned(zoned) = to_temporal_in(timed, &SystemZone::utc()) else {
            panic!("a clock crosses the zone seam and is stored as an instant");
        };
        assert_eq!(
            zoned.instant_ms(),
            NaiveDate::from_ymd_opt(2026, 8, 3)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
            "resolved in the zone that was handed in"
        );
    }

    /// **A signed clock duration on a selected future day counts from that
    /// day**, not from today — the reference the calendar supplies is what
    /// decides (a bare `9am` is today's by the grammar's own rule; `+9h` is
    /// the selection's), and a surface that parsed the same words against a
    /// fresh clock would store a different deadline while looking identical.
    #[test]
    fn a_timed_phrase_on_a_selected_day_counts_from_that_day() {
        let on_far = Reference::on_day(now(), far());
        let timed = parse("+9h", on_far, DEFAULT_DATE_ORDER).unwrap();
        let Temporal::Zoned(zoned) = to_temporal_in(timed, &SystemZone::utc()) else {
            panic!("a clock is an instant");
        };
        assert_eq!(
            zoned.instant_ms(),
            far()
                .and_hms_opt(19, 15, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
            "nine hours past the clock's time of day, on the highlighted day"
        );
        // And the same words against a clock with no selection count from
        // today.
        let today = parse(
            "+9h",
            Reference::without_selection(now()),
            DEFAULT_DATE_ORDER,
        )
        .unwrap();
        let Temporal::Zoned(from_today) = to_temporal_in(today, &SystemZone::utc()) else {
            panic!("a clock is an instant");
        };
        assert_eq!(
            from_today.instant_ms(),
            now().and_utc().timestamp_millis() + 9 * 3_600_000,
            "which is exactly why the reference is the caller's to supply"
        );
    }

    /// The public door reads the machine's zone; the mapping it applies is the
    /// injected one's, so the two agree on the date arm without a machine in
    /// the loop.
    #[test]
    fn the_public_door_and_the_injected_one_agree_on_a_date() {
        let at = Reference::without_selection(now());
        let day = parse("in 3d", at, DEFAULT_DATE_ORDER).unwrap();
        assert_eq!(to_temporal(day), to_temporal_in(day, &SystemZone::utc()));
    }

    /// The `/due` parser edge's complete pre-Piece-1 mapping contract. A day
    /// must remain date-only, a clock must remain floating until the due
    /// commit resolves it, and unknown input must keep opening the picker.
    #[test]
    fn due_mapping_regression_goldens() {
        let at = Reference::without_selection(now());
        let cases = [
            (
                "tomorrow",
                Some(Parsed::Date(DateOnly::new(
                    NaiveDate::from_ymd_opt(2026, 8, 3).unwrap(),
                ))),
            ),
            (
                "tomorrow 9am",
                Some(Parsed::Floating(FloatingDateTime::new(
                    NaiveDate::from_ymd_opt(2026, 8, 3)
                        .unwrap()
                        .and_hms_opt(9, 0, 0)
                        .unwrap(),
                ))),
            ),
            (
                "in 90m",
                Some(Parsed::Floating(FloatingDateTime::new(
                    NaiveDate::from_ymd_opt(2026, 8, 2)
                        .unwrap()
                        .and_hms_opt(11, 45, 0)
                        .unwrap(),
                ))),
            ),
            ("someday", None),
            ("tomorrow 9am extra", None),
        ];

        for (input, expected) in cases {
            assert_eq!(parse(input, at, DEFAULT_DATE_ORDER), expected, "{input:?}");
        }
    }

    #[test]
    fn all_eight_languages_resolve_to_the_same_canonical_reminder() {
        let phrases = [
            (Language::English, "tomorrow at 9am"),
            (Language::Spanish, "mañana 9:00"),
            (Language::PortugueseBrazil, "amanhã 9:00"),
            (Language::German, "morgen 9:00"),
            (Language::French, "demain 9:00"),
            (Language::Arabic, "غدًا 9:00"),
            (Language::ChineseSimplified, "明天9:00"),
            (Language::Japanese, "明日9:00"),
        ];
        let expected = Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
            NaiveDate::from_ymd_opt(2026, 8, 3)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )));
        for (language, phrase) in phrases {
            assert_eq!(
                parse_reminder(phrase, &context(language)),
                expected,
                "{phrase}"
            );
        }

        let durations = [
            (Language::English, "in 2 hours"),
            (Language::Spanish, "en 2 horas"),
            (Language::PortugueseBrazil, "em 2 horas"),
            (Language::German, "in 2 stunden"),
            (Language::French, "dans 2 heures"),
            (Language::Arabic, "بعد 2 ساعة"),
            (Language::ChineseSimplified, "2小时后"),
            (Language::Japanese, "2時間後"),
        ];
        let duration_expected =
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 2)
                    .unwrap()
                    .and_hms_opt(12, 15, 0)
                    .unwrap(),
            )));
        for (language, phrase) in durations {
            assert_eq!(
                parse_reminder(phrase, &context(language)),
                duration_expected,
                "{phrase}"
            );
        }

        let before_due = [
            (Language::English, "2 hours before due"),
            (Language::Spanish, "2 horas antes del vencimiento"),
            (Language::PortugueseBrazil, "2 horas antes do vencimento"),
            (Language::German, "2 stunden vor fälligkeit"),
            (Language::French, "2 heures avant l'échéance"),
            (Language::Arabic, "2 ساعة قبل الاستحقاق"),
            (Language::ChineseSimplified, "2小时到期前"),
            (Language::Japanese, "2時間期限前"),
        ];
        let relative_expected = Resolution::Resolved(ReminderDraft::Relative {
            relation: koru_when::DueRelation::Before,
            offset: Some(DurationExpression {
                amount: 2,
                unit: DurationUnit::Hour,
                origin: DurationOrigin::Now,
                sign: DurationSign::Forward,
            }),
            anchor: None,
            preview: None,
        });
        for (language, phrase) in before_due {
            assert_eq!(
                parse_reminder(phrase, &context(language)),
                relative_expected,
                "{phrase}"
            );
        }

        for language in Language::ALL {
            assert_eq!(
                parse_reminder("10/28", &context(language)),
                Resolution::Resolved(ReminderDraft::Floating(
                    DateOnly::new(NaiveDate::from_ymd_opt(2026, 10, 28).unwrap())
                        .at(default_reminder_time()),
                )),
                "{} date-only canonical output",
                language.tag()
            );
        }
    }

    #[test]
    fn relative_reminders_land_on_the_minute_the_ui_displays() {
        let mut context = context(Language::English);
        context.now_ms = NaiveDate::from_ymd_opt(2026, 9, 5)
            .unwrap()
            .and_hms_milli_opt(14, 23, 52, 734)
            .unwrap()
            .and_utc()
            .timestamp_millis();

        assert_eq!(
            parse_reminder("in 1 min", &context),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 9, 5)
                    .unwrap()
                    .and_hms_opt(14, 24, 0)
                    .unwrap(),
            )))
        );
        assert_eq!(
            parse_due("in 1 min", &context),
            Resolution::Resolved(Temporal::Zoned(ZonedDateTime::new(
                context.now_ms + 60_000,
                TimeZoneName::utc(),
            ))),
            "shared due values retain exact duration semantics"
        );
        assert_eq!(
            parse_reminder("in 1 hour", &context),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 9, 5)
                    .unwrap()
                    .and_hms_opt(15, 23, 0)
                    .unwrap(),
            )))
        );
    }

    #[test]
    fn next_week_uses_the_effective_first_day_and_nine_in_the_morning() {
        let mut monday = context(Language::English);
        monday.system_week_start = WeekStart::Monday;
        assert_eq!(
            parse_reminder("next week", &monday),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            )))
        );

        let mut sunday = monday.clone();
        sunday.system_week_start = WeekStart::Sunday;
        assert_eq!(
            parse_reminder("next week", &sunday),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 9)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            )))
        );

        sunday.week_start_override = Some(WeekStart::Monday);
        assert_eq!(
            parse_reminder("next week", &sunday),
            parse_reminder("next week", &monday)
        );
    }

    #[test]
    fn localized_next_week_phrases_resolve_to_the_same_reminder() {
        let phrases = [
            (Language::English, "next week"),
            (Language::Spanish, "la próxima semana"),
            (Language::PortugueseBrazil, "próxima semana"),
            (Language::German, "nächste woche"),
            (Language::French, "la semaine prochaine"),
            (Language::Arabic, "الأسبوع القادم"),
            (Language::ChineseSimplified, "下周"),
            (Language::Japanese, "来週"),
        ];
        let expected = parse_reminder("next week", &context(Language::English));
        for (language, phrase) in phrases {
            assert_eq!(
                parse_reminder(phrase, &context(language)),
                expected,
                "{phrase}"
            );
        }
    }

    #[test]
    fn date_only_and_dayparts_become_visible_floating_drafts() {
        assert_eq!(
            parse_reminder("tomorrow", &context(Language::English)),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            )))
        );
        for (phrase, hour) in [
            ("tomorrow morning", 9),
            ("tomorrow afternoon", 15),
            ("tomorrow evening", 19),
        ] {
            assert_eq!(
                parse_reminder(phrase, &context(Language::English)),
                Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                    NaiveDate::from_ymd_opt(2026, 8, 3)
                        .unwrap()
                        .and_hms_opt(hour, 0, 0)
                        .unwrap(),
                ))),
                "{phrase}"
            );
        }
    }

    #[test]
    fn tonight_rolls_only_after_nineteen_hundred() {
        let mut early = context(Language::English);
        early.now_ms = NaiveDate::from_ymd_opt(2026, 8, 2)
            .unwrap()
            .and_hms_opt(18, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let mut late = early.clone();
        late.now_ms = NaiveDate::from_ymd_opt(2026, 8, 2)
            .unwrap()
            .and_hms_opt(20, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let expected = |day| {
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, day)
                    .unwrap()
                    .and_hms_opt(19, 0, 0)
                    .unwrap(),
            )))
        };
        assert_eq!(parse_reminder("tonight", &early), expected(2));
        assert_eq!(parse_reminder("tonight", &late), expected(3));
    }

    #[test]
    fn explicit_zones_are_canonical_and_abbreviations_are_questions() {
        let context = context(Language::English);
        let Resolution::Resolved(ReminderDraft::Zoned(value)) =
            parse_reminder("tomorrow 9am America/New_York", &context)
        else {
            panic!("expected zoned reminder")
        };
        assert_eq!(value.zone().as_str(), "America/New_York");
        assert_eq!(value.instant_ms(), 1_785_762_000_000);
        assert_eq!(
            parse_reminder("tomorrow 9am EST", &context),
            Resolution::Needs(ResolutionIssue::AmbiguousZone)
        );
        let Resolution::Resolved(ReminderDraft::Zoned(alias)) =
            parse_reminder("tomorrow 9am US/Eastern", &context)
        else {
            panic!("a database-backed alias should resolve")
        };
        assert_eq!(alias.zone().as_str(), "America/New_York");
        assert_eq!(
            parse_reminder("tomorrow 9am Mars/Olympus", &context),
            Resolution::Needs(ResolutionIssue::UnknownZone)
        );
    }

    #[test]
    fn user_controlled_arithmetic_refuses_bounds_instead_of_panicking() {
        let context = context(Language::English);
        assert_eq!(
            parse_reminder("next blue moon", &context),
            Resolution::Needs(ResolutionIssue::Unsupported),
            "the structured editor can fall back without receiving prose"
        );
        let duration = TemporalExpression::Duration(DurationExpression {
            amount: u32::MAX,
            unit: DurationUnit::Day,
            origin: DurationOrigin::Now,
            sign: DurationSign::Forward,
        });
        assert_eq!(
            resolve_reminder(&duration, &context),
            Resolution::Needs(ResolutionIssue::Unsupported)
        );
        let malformed_duration = |amount, origin, sign| {
            TemporalExpression::Duration(DurationExpression {
                amount,
                unit: DurationUnit::Hour,
                origin,
                sign,
            })
        };
        for malformed in [
            malformed_duration(0, DurationOrigin::Now, DurationSign::Forward),
            malformed_duration(2, DurationOrigin::Now, DurationSign::Backward),
            malformed_duration(2, DurationOrigin::Selection, DurationSign::Forward),
        ] {
            assert_eq!(
                resolve_reminder(&malformed, &context),
                Resolution::Needs(ResolutionIssue::Unsupported),
                "a manually constructed duration cannot lose its reference semantics"
            );
        }
        assert_eq!(
            resolve_reminder(
                &TemporalExpression::Date(DateExpression::Weekday(7)),
                &context,
            ),
            Resolution::Needs(ResolutionIssue::Unsupported),
            "a malformed public expression is refused rather than normalized"
        );

        let anchor = Temporal::Zoned(ZonedDateTime::new(
            i64::MIN,
            TimeZoneName::parse("America/New_York").unwrap(),
        ));
        let offset = DurationExpression {
            amount: 1,
            unit: DurationUnit::Minute,
            origin: DurationOrigin::Now,
            sign: DurationSign::Forward,
        };
        assert_eq!(
            relative_preview(&anchor, koru_when::DueRelation::Before, Some(offset)),
            Err(ResolutionIssue::Unsupported)
        );
    }

    #[test]
    fn end_of_day_requires_the_injected_setting() {
        let mut context = context(Language::English);
        assert_eq!(
            parse_reminder("end of day", &context),
            Resolution::Needs(ResolutionIssue::MissingEndOfDay)
        );
        context.end_of_day = NaiveTime::from_hms_opt(17, 30, 0);
        assert_eq!(
            parse_reminder("end of day", &context),
            Resolution::Resolved(ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 2)
                    .unwrap()
                    .and_hms_opt(17, 30, 0)
                    .unwrap(),
            )))
        );
    }

    #[test]
    fn past_input_returns_a_visible_next_day_proposal() {
        let Resolution::Passed {
            requested,
            proposal,
        } = parse_reminder("yesterday 9am", &context(Language::English))
        else {
            panic!("expected passed-time proposal")
        };
        assert_eq!(
            requested,
            ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 1)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            ))
        );
        assert_eq!(
            proposal,
            ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            ))
        );

        let Resolution::Passed { proposal, .. } =
            parse_reminder("yesterday 5pm", &context(Language::English))
        else {
            panic!("an old explicit day should offer its next future wall time")
        };
        assert_eq!(
            proposal,
            ReminderDraft::Floating(FloatingDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, 2)
                    .unwrap()
                    .and_hms_opt(17, 0, 0)
                    .unwrap(),
            ))
        );
    }

    #[test]
    fn locale_date_order_is_an_injected_resolution_input() {
        let month_first = context(Language::English);
        let mut day_first = month_first.clone();
        day_first.date_order = DateOrder::DayFirst;
        assert_eq!(
            parse_reminder("10/11", &month_first),
            Resolution::Resolved(ReminderDraft::Floating(
                DateOnly::new(NaiveDate::from_ymd_opt(2026, 10, 11).unwrap())
                    .at(default_reminder_time())
            ))
        );
        assert_eq!(
            parse_reminder("10/11", &day_first),
            Resolution::Resolved(ReminderDraft::Floating(
                DateOnly::new(NaiveDate::from_ymd_opt(2026, 11, 10).unwrap())
                    .at(default_reminder_time())
            ))
        );
    }

    #[test]
    fn at_and_before_due_keep_anchor_semantics() {
        let rules = IanaZoneRules::parse("America/New_York").unwrap();
        let due_wall = FloatingDateTime::new(
            NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        );
        let mut context = context(Language::English);
        context.due_anchor = Some(Temporal::Zoned(due_wall.in_zone(&rules)));

        let Resolution::Resolved(ReminderDraft::Relative {
            preview: Some(ReminderMoment::Zoned(day_before)),
            ..
        }) = parse_reminder("1 day before due", &context)
        else {
            panic!("calendar day preview")
        };
        assert_eq!(day_before.wall_in(&rules).wall().hour(), 9);
        assert_eq!(
            day_before.wall_in(&rules).wall().date(),
            NaiveDate::from_ymd_opt(2026, 3, 7).unwrap()
        );

        let Resolution::Resolved(ReminderDraft::Relative {
            preview: Some(ReminderMoment::Zoned(hours_before)),
            ..
        }) = parse_reminder("24 hours before due", &context)
        else {
            panic!("exact hour preview")
        };
        assert_eq!(hours_before.wall_in(&rules).wall().hour(), 8);
        assert_eq!(hours_before.zone().as_str(), "America/New_York");

        context.due_anchor = None;
        assert!(matches!(
            parse_reminder("at due", &context),
            Resolution::Resolved(ReminderDraft::Relative {
                relation: koru_when::DueRelation::At,
                anchor: None,
                preview: None,
                ..
            })
        ));
        assert_eq!(
            parse_reminder("1 month before due", &context),
            Resolution::Needs(ResolutionIssue::Unsupported)
        );

        for sign in [DurationSign::Forward, DurationSign::Backward] {
            let invalid = TemporalExpression::Due(DueExpression::Before(DurationExpression {
                amount: 2,
                unit: DurationUnit::Hour,
                origin: DurationOrigin::Selection,
                sign,
            }));
            assert_eq!(
                resolve_reminder(&invalid, &context),
                Resolution::Needs(ResolutionIssue::Unsupported),
                "a direct unresolved value cannot reinterpret selection-relative intent"
            );
        }
        let zero = TemporalExpression::Due(DueExpression::Before(DurationExpression {
            amount: 0,
            unit: DurationUnit::Hour,
            origin: DurationOrigin::Now,
            sign: DurationSign::Forward,
        }));
        assert_eq!(
            resolve_reminder(&zero, &context),
            Resolution::Needs(ResolutionIssue::Unsupported)
        );
    }

    #[test]
    fn due_mapping_returns_date_or_canonical_zoned_value() {
        let context = context(Language::English);
        assert!(matches!(
            parse_due("tomorrow", &context),
            Resolution::Resolved(Temporal::Date(_))
        ));
        let Resolution::Resolved(Temporal::Zoned(value)) = parse_due("tomorrow 9am", &context)
        else {
            panic!("timed due must be zoned")
        };
        assert_eq!(value.zone().as_str(), "UTC");
        assert_eq!(value.instant_ms(), 1_785_747_600_000);
    }
}
