// SPDX-License-Identifier: MPL-2.0

//! **Natural-language dates, times and durations — the grammar reference.**
//!
//! [`parse_expression`] answers what the person typed and nothing more: an
//! unresolved date, wall time, duration, daypart, explicit zone token, or
//! at/before-due relation. It never answers an instant and never names a Koru
//! model type. The compatibility [`parse`] API retains the original concrete
//! English `When::{Date, Wall}` behavior used by `/due`; its frozen goldens are
//! the boundary for future grammar work.
//!
//! What a phrase *means* — which attribute it lands on, whether it floats or
//! is shared and zoned, and what "now" or "due" means — is decided by the
//! caller with explicit context ([docs/architecture/TIME.md](../../../docs/architecture/TIME.md), and
//! ADR-183 rule 4). This crate cannot answer those questions, and that is the
//! point: it has no model, settings, storage, process, environment, or I/O
//! dependency.
//!
//! **Nothing here reads a clock.** The legacy API's two reference points arrive
//! as arguments (see [`Reference`]); unresolved expressions need no reference
//! at all. Both APIs are plain deterministic functions.
//!
//! ---
//!
//! # The grammar
//!
//! Input is lowercased, split on whitespace, and commas are dropped. A lone
//! `am`/`pm` is glued onto the number in front of it, so `5 pm` and `5pm` are
//! one token by the time any rule below runs — there is exactly one definition
//! of the token stream, in `tokenize`.
//!
//! ## Days
//!
//! | Phrase | Answers |
//! | --- | --- |
//! | `today`, `tomorrow`, `yesterday` | that day |
//! | `friday`, `fri`, `thurs` | the **next strict** occurrence — `friday` typed on a Friday is next Friday |
//! | `this friday`, `coming fri` | the same: the modifier says what the bare weekday already does |
//! | `next friday`, `next fri` | that weekday **in next week** — Thursday's *next friday* is eight days on, not one |
//! | `aug 31`, `august 31`, `31 aug`, `sept 10` | month and day, either way round |
//! | `aug 31 2027` | with an explicit four-digit year |
//! | `10/28`, `28/10`, `10/28/2027`, `2027-10-28` | numeric dates — see below |
//!
//! A month name may be abbreviated to any prefix of three characters or more,
//! so three-letter *and* four-letter forms parse from one rule and `sept` is
//! not a special case (see [`vocab`]).
//!
//! **A yearless date rolls forward.** `jan 5` typed in August means next
//! January: nobody sets a due date behind them by naming a month. A roll that
//! lands on a day that does not exist — `feb 29` rolling into a common year —
//! is **refused** rather than moved to March.
//!
//! ## Times
//!
//! `9am`, `9 am`, `9:30pm`, `17:30`, after any day phrase, with an optional
//! `at`: `tomorrow 9am`, `friday at 17:30`, `aug 31 at 9:15 pm`.
//!
//! A time may also stand alone — `5pm`, `17:30` — and means today at that
//! hour, or **tomorrow** at it once the moment has gone by. The comparison is
//! strict: `10:15` typed at 10:15 is tomorrow, because a due date one minute in
//! the past is nobody's intention.
//!
//! **A bare number is never a time.** `5`, `31`, `2026` and `+31` are all
//! refused. A standalone clock needs a meridiem or a colon, or a day-of-month
//! fragment would silently become an hour.
//!
//! ## Durations, and the two reference points
//!
//! `30m`, `4h`, `3d`, `2w`, `2mo`, with the unit spelled out (`2 weeks`) or
//! glued (`2w`), and spaced or not (`4 h`).
//!
//! **The sign chooses which reference point the duration counts from**, and
//! that is the one place this grammar is not the editor's older one:
//!
//! | Phrase | Counted from |
//! | --- | --- |
//! | `2w`, `in 2w` | **now** — [`Reference::now`] |
//! | `+2w` | **the selection**, forwards — [`Reference::selection`] |
//! | `-2w` | **the selection**, backwards |
//!
//! `+` used to be decoration: `+4h` and `4h` were the same phrase. They are
//! not any more. A phrase box under a calendar has a highlighted day beside
//! it, and `+1w` typed there means *a week from what I am looking at* — which
//! is the only reading under which the picker and the box agree about what is
//! on screen. With the selection sitting at today's date and time the two
//! spellings still coincide, so the change is invisible until a selection
//! moves.
//!
//! `in` and a sign contradict each other, so `in +2w` is refused rather than
//! half-understood.
//!
//! **Day-and-longer durations answer a calendar day; sub-day ones answer a
//! wall clock.** `in 3d` is a day, not 10:15 three days from now; `in 90m` is a
//! reading on a clock. Month arithmetic clamps the day: Jan 31 + 1mo is Feb 28.
//!
//! ## Numeric dates
//!
//! `10/28` and `28/10`, optionally with a **four-digit year in third
//! position under month-first or day-first order: `10/28/2027`,
//! `28/10/2027`. Year-first order instead requires a four-digit leading year
//! and accepts `2027-10-28` and `2027/10/28`. Two-digit years are always
//! refused because `27` is also a day.
//!
//! **The order is a parameter, not a constant.** [`DateOrder`] is an argument
//! to [`parse`]. Locale-aware callers supply the locale's field order; callers
//! without one use [`DEFAULT_DATE_ORDER`].
//!
//! It is only consulted when the pair is **genuinely ambiguous**. If one number
//! cannot be a month and the other can, the unambiguous reading wins under
//! either setting: `28/10` and `13/10` are October 28 and October 13 whatever
//! the preference says. Only a pair where both readings are legal — `10/11` —
//! follows it. Year-first order refuses an ambiguous yearless pair rather than
//! borrowing month-first or day-first semantics that the locale did not express.
//!
//! ---
//!
//! # What is deliberately not grammar
//!
//! `last tuesday`, `end of month`, a trailing period on an abbreviation
//! (`sept.`), and every language that is not English. Each is a table entry or
//! a small arm away; none has been asked for, and a form that half-parses is
//! worse than one that opens the picker.

mod calendar;
mod expression;
pub mod vocab;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};

pub use expression::{
    DateExpression, Daypart, DueExpression, DueRelation, DurationExpression, DurationOrigin,
    DurationSign, Language, TemporalExpression, TimeExpression, parse_expression,
    parse_expression_in, parse_reminder_expression,
};
pub use vocab::{DurationUnit, ENGLISH, Vocabulary};

/// What a phrase resolved to.
///
/// **There is no third arm, and neither of these can carry a zone.** That is
/// this crate's whole contribution to the temporal model: it records what the
/// person said and hands the zone question upwards. Callers in Koru map
/// [`When::Date`] onto `koru_core::time::DateOnly` and [`When::Wall`] onto
/// `FloatingDateTime`; the mapping is theirs because the model is theirs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum When {
    /// A calendar day: `friday`, `aug 31`, `in 3d`, `10/28`.
    Date(NaiveDate),
    /// A wall-clock reading: `tomorrow 9am`, `in 90m`, `+4h`. Floating — it
    /// says nothing about where the person is.
    Wall(NaiveDateTime),
}

/// Which number a slash date puts first.
///
/// A preference in waiting: it is an argument rather than a constant so the
/// day the user is offered the choice, nothing in the grammar has to move.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DateOrder {
    /// `10/28` is October 28.
    MonthFirst,
    /// `10/28` is… still October 28, because 28 is not a month. Only a pair
    /// that reads both ways — `10/11` — is November 10 here.
    DayFirst,
    /// A complete numeric date puts its four-digit year first. An ambiguous
    /// yearless pair is refused because this order says nothing about which of
    /// its remaining fields is the month; an unambiguous pair still parses.
    YearFirst,
}

/// **The one place the default lives.** Flipping the product's default slash
/// order is an edit to this line and nothing else.
pub const DEFAULT_DATE_ORDER: DateOrder = DateOrder::MonthFirst;

impl Default for DateOrder {
    fn default() -> Self {
        DEFAULT_DATE_ORDER
    }
}

/// The two points a phrase can be relative to.
///
/// **They are different questions and a bug that confuses them is invisible**
/// until a selection moves off today, which is why they are two fields rather
/// than one and why the tests always set them apart.
///
/// - `now` is the clock: `today`, `friday`, a bare `5pm` and an unsigned
///   duration are all relative to it.
/// - `selection` is the day the caller's calendar is highlighting. Only a
///   signed duration (`+2w`, `-2w`) is relative to it.
///
/// `selection` carries a wall clock because `+4h` has to land on one. A caller
/// whose selection is a bare day builds the reference with
/// [`Reference::on_day`], which puts it at the current time of day — so with
/// today selected, `+4h` and `4h` agree.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Reference {
    /// The injected clock. Nothing in this crate reads a real one.
    pub now: NaiveDateTime,
    /// The day (and clock) a signed duration counts from.
    pub selection: NaiveDateTime,
}

impl Reference {
    pub fn new(now: NaiveDateTime, selection: NaiveDateTime) -> Self {
        Self { now, selection }
    }

    /// A selection that is a **day**: read at `now`'s time of day, so a signed
    /// sub-day duration has a clock to move along and `+4h` on today's
    /// selection means what `4h` means.
    pub fn on_day(now: NaiveDateTime, selection: NaiveDate) -> Self {
        Self {
            now,
            selection: selection.and_time(now.time()),
        }
    }

    /// Nothing selected: signed durations fall back to the clock, which makes
    /// `+2w` and `2w` the same phrase again.
    pub fn without_selection(now: NaiveDateTime) -> Self {
        Self {
            now,
            selection: now,
        }
    }
}

/// Parse `input` in English. `None` means the phrase is not one this grammar
/// knows — the caller opens a picker rather than guessing.
pub fn parse(input: &str, at: Reference, order: DateOrder) -> Option<When> {
    parse_in(input, at, order, &ENGLISH)
}

/// [`parse`], against an explicit [`Vocabulary`].
///
/// The localization seam. There is one table today; this signature is what
/// makes a second one a table rather than a rewrite.
pub fn parse_in(input: &str, at: Reference, order: DateOrder, words: &Vocabulary) -> Option<When> {
    let lowered = input.trim().to_lowercase();
    let joined = tokenize(&lowered, words);
    let tokens: Vec<&str> = joined.iter().map(String::as_str).collect();
    if tokens.is_empty() {
        return None;
    }

    // `in <duration>` — always from now, so a sign here contradicts the word
    // in front of it and is refused rather than silently preferred.
    if Vocabulary::is(words.in_word, tokens[0]) {
        let (amount, unit, sign) = duration(&tokens[1..], words)?;
        if sign != Sign::Bare {
            return None;
        }
        return shift(at.now, amount, unit);
    }

    // A duration with no `in`: `4h`, `+4h`, `-2w`, `2 weeks`. Tried **before**
    // the date phrase and only when the first token opens with a digit (after
    // an optional sign), which is what keeps it away from `31 aug` and
    // `2 may` — both reach `duration`, neither names a unit, and both fall
    // through to the month-day reading below.
    let head = tokens[0];
    let signed = head.starts_with(['+', '-']);
    let stem = head.strip_prefix(['+', '-']).unwrap_or(head);
    if stem.starts_with(|c: char| c.is_ascii_digit()) {
        if let Some((amount, unit, sign)) = duration(&tokens, words) {
            return match sign {
                Sign::Bare => shift(at.now, amount, unit),
                Sign::Plus => shift(at.selection, amount, unit),
                Sign::Minus => shift(at.selection, -amount, unit),
            };
        }
        if signed {
            // A sign opens a duration and nothing else, so `+31` and `+friday`
            // are refused here rather than falling into the date phrase.
            return None;
        }
    }

    // A **bare time**: `5pm`, `9am`, `17:30`. Tried only once the date phrase
    // has declined, so nothing that already names a day changes meaning — and
    // only over a single token, because `31 aug` is a date whose first token
    // would otherwise be offered to the clock reader.
    let Some((date, used)) = date_phrase(&tokens, at.now.date(), order, words) else {
        return match tokens.as_slice() {
            [tok] => clock(tok, words).map(|t| When::Wall(today_or_tomorrow(at.now, t))),
            _ => None,
        };
    };
    let rest = &tokens[used..];
    if rest.is_empty() {
        return Some(When::Date(date));
    }
    // An optional `at`, then exactly one time token — anything more is a
    // phrase this grammar does not know, and half-understanding it would
    // commit a date the person did not say.
    let rest = if Vocabulary::is(words.at_word, rest[0]) {
        &rest[1..]
    } else {
        rest
    };
    match rest {
        [tok] => clock(tok, words).map(|t| When::Wall(date.and_time(t))),
        _ => None,
    }
}

/// Which reference point a duration counts from, read off its sign.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Sign {
    /// No sign: from `now`.
    Bare,
    /// `+`: forwards from the selection.
    Plus,
    /// `-`: backwards from the selection.
    Minus,
}

/// Whitespace-separated, comma-stripped — **and a lone `am`/`pm` glued onto
/// the number in front of it**, so `5 pm` arrives as the one token `5pm` and
/// `aug 31 at 9 pm` as `["aug", "31", "at", "9pm"]`.
///
/// **This is the only definition of the token stream.** Every form gets the
/// spaced spelling from one rule — the bare time, the attached time and the
/// `at` variant alike — and no arm of the grammar has to know a space was
/// typed.
///
/// It cannot weaken the bare-number guard, because it only ever *adds* a
/// meridiem: `5` alone is still `5` and still refused; `31 aug` is untouched,
/// since `aug` is not a meridiem; a `pm` with nothing in front of it stays its
/// own token and reaches nothing that reads it. What may be glued to is
/// deliberately narrow — [`is_clock_digits`] — so `+5 pm` and `5 pm pm` are
/// refused rather than half-understood.
fn tokenize(lowered: &str, words: &Vocabulary) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in lowered.split_whitespace().map(|t| t.trim_matches(',')) {
        if token.is_empty() {
            continue;
        }
        if words.is_meridiem(token)
            && let Some(prev) = out.last_mut()
            && is_clock_digits(prev)
        {
            prev.push_str(token);
            continue;
        }
        out.push(token.to_owned());
    }
    out
}

/// The numeric shape a spaced `am`/`pm` may attach to: `5`, `12`, `10:15`.
///
/// Digits and colons only, opening with a digit — so a sign prefix, a month
/// name, a slash date and an already-glued `5pm` are all left alone, and the
/// second `pm` of `5 pm pm` finds nothing to join.
fn is_clock_digits(token: &str) -> bool {
    token.starts_with(|c: char| c.is_ascii_digit())
        && token.chars().all(|c| c.is_ascii_digit() || c == ':')
}

/// `<n> <unit>` (or the glued `2w`), with any leading `in` already taken off by
/// the caller and the sign reported rather than discarded — so `in 4h`, `4h`,
/// `+4h` and `-4h` are one code path and cannot drift apart.
///
/// The amount is bounded at 10,000 of whatever the unit is: past that the
/// person is not naming a due date, and a huge month shift is a slow way to
/// find chrono's limits.
fn duration(rest: &[&str], words: &Vocabulary) -> Option<(i64, DurationUnit, Sign)> {
    let first = *rest.first()?;
    let (sign, first) = match first.strip_prefix('+') {
        Some(body) => (Sign::Plus, body),
        None => match first.strip_prefix('-') {
            Some(body) => (Sign::Minus, body),
            None => (Sign::Bare, first),
        },
    };
    let (amount, unit_token): (i64, &str) = match rest {
        [_] => split_amount_unit(first)?,
        [_, unit] => (first.parse().ok()?, unit),
        _ => return None,
    };
    if amount <= 0 || amount > 10_000 {
        return None;
    }
    Some((amount, words.unit(unit_token)?, sign))
}

/// `2w` → `(2, "w")`. The unit is whatever trails the digits.
fn split_amount_unit(token: &str) -> Option<(i64, &str)> {
    let split = token.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = token.split_at(split);
    Some((digits.parse().ok()?, unit))
}

/// Move `base` by a signed `amount` of `unit`, and answer the *kind* of value
/// that implies: a wall clock below a day, a calendar date at a day and above.
fn shift(base: NaiveDateTime, amount: i64, unit: DurationUnit) -> Option<When> {
    Some(match unit {
        DurationUnit::Minute => When::Wall(base.checked_add_signed(TimeDelta::minutes(amount))?),
        DurationUnit::Hour => When::Wall(base.checked_add_signed(TimeDelta::hours(amount))?),
        DurationUnit::Day => When::Date(calendar::shift_days(base.date(), amount)),
        DurationUnit::Week => When::Date(calendar::shift_days(base.date(), amount.checked_mul(7)?)),
        DurationUnit::Month => When::Date(calendar::shift_month(
            base.date(),
            i32::try_from(amount).ok()?,
        )),
    })
}

/// The date phrase at the front of `tokens`, and how many tokens it used.
fn date_phrase(
    tokens: &[&str],
    today: NaiveDate,
    order: DateOrder,
    words: &Vocabulary,
) -> Option<(NaiveDate, usize)> {
    let head = tokens[0];
    if Vocabulary::is(words.today, head) {
        return Some((today, 1));
    }
    if Vocabulary::is(words.tomorrow, head) {
        return Some((calendar::shift_days(today, 1), 1));
    }
    if Vocabulary::is(words.yesterday, head) {
        return Some((calendar::shift_days(today, -1), 1));
    }
    // A weekday with its modifier — `next friday`, `this friday` — before
    // the bare weekday, which is the modifier's own head word.
    if let Some(index) = tokens.get(1).and_then(|second| words.weekday(second)) {
        if Vocabulary::is(words.next_word, head) {
            return Some((weekday_next_week(today, index), 2));
        }
        if Vocabulary::is(words.this_word, head) {
            return Some((next_weekday(today, index), 2));
        }
    }
    if let Some(index) = words.weekday(head) {
        return Some((next_weekday(today, index), 1));
    }
    if head.contains(['/', '-']) {
        return numeric_date(head, today, order).map(|d| (d, 1));
    }
    // Month-day, either way round, with an optional third year token.
    let (month, day_token) = match (words.month(head), tokens.get(1)) {
        (Some(m), Some(day)) => (m, *day),
        (None, Some(second)) => (words.month(second)?, head),
        _ => return None,
    };
    let day: u32 = day_token.parse().ok().filter(|d| (1..=31).contains(d))?;
    if let Some(year_token) = tokens.get(2)
        && year_token.len() == 4
        && let Ok(year) = year_token.parse::<i32>()
    {
        return Some((NaiveDate::from_ymd_opt(year, month, day)?, 3));
    }
    Some((roll_forward(today, month, day)?, 2))
}

/// `10/28`, `28/10`, `10/28/2027`.
///
/// Month/day orders accept a four-digit trailing year. Year-first accepts a
/// four-digit leading year; only unambiguous pairs may omit it.
#[derive(Copy, Clone)]
struct NumericDate {
    year: Option<i32>,
    month: u32,
    day: u32,
}

/// Decode the numeric field shape once for both public parser APIs.
fn numeric_date_parts(token: &str, order: DateOrder) -> Option<NumericDate> {
    if order == DateOrder::YearFirst {
        let separator = if token.contains('-') { '-' } else { '/' };
        let mut parts = token.split(separator);
        let first = parts.next()?;
        if first.len() == 4 && first.bytes().all(|byte| byte.is_ascii_digit()) {
            let month = small_number(parts.next()?).filter(|value| (1..=12).contains(value))?;
            let day = small_number(parts.next()?).filter(|value| (1..=31).contains(value))?;
            if parts.next().is_some() {
                return None;
            }
            return Some(NumericDate {
                year: Some(first.parse().ok()?),
                month,
                day,
            });
        }
        if separator != '/' {
            return None;
        }
        let first = small_number(first)?;
        let second = small_number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let (month, day) = disambiguate(first, second, order)?;
        return Some(NumericDate {
            year: None,
            month,
            day,
        });
    }

    if !token.contains('/') {
        return None;
    }
    let mut parts = token.split('/');
    let first = small_number(parts.next()?)?;
    let second = small_number(parts.next()?)?;
    let year = match parts.next() {
        Some(value) if value.len() == 4 => Some(value.parse().ok()?),
        Some(_) => return None,
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    let (month, day) = disambiguate(first, second, order)?;
    Some(NumericDate { year, month, day })
}

fn numeric_date(token: &str, today: NaiveDate, order: DateOrder) -> Option<NaiveDate> {
    let NumericDate { year, month, day } = numeric_date_parts(token, order)?;
    match year {
        Some(year) => NaiveDate::from_ymd_opt(year, month, day),
        None => roll_forward(today, month, day),
    }
}

/// One or two digits, as a number. Longer is a year in the wrong place.
fn small_number(part: &str) -> Option<u32> {
    if part.is_empty() || part.len() > 2 || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Which of a slash pair is the month.
///
/// **The setting is the tie-breaker, not the rule.** A pair that reads only one
/// way reads that way under either setting — `28/10` is October 28 for
/// everybody, and a preference that turned it into "the 28th month" would be a
/// preference for being wrong. Only a pair where both numbers could be months
/// consults `order`.
fn disambiguate(first: u32, second: u32, order: DateOrder) -> Option<(u32, u32)> {
    let month_first = (1..=12).contains(&first) && (1..=31).contains(&second);
    let day_first = (1..=12).contains(&second) && (1..=31).contains(&first);
    match (month_first, day_first) {
        (true, true) => Some(match order {
            DateOrder::MonthFirst => (first, second),
            DateOrder::DayFirst => (second, first),
            DateOrder::YearFirst => return None,
        }),
        (true, false) => Some((first, second)),
        (false, true) => Some((second, first)),
        (false, false) => None,
    }
}

/// A yearless date: this year's, rolled to next year when it is already
/// behind us. `None` when neither year has that day — `feb 29` in a run of
/// common years is refused rather than moved to March.
fn roll_forward(today: NaiveDate, month: u32, day: u32) -> Option<NaiveDate> {
    let this_year = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if this_year < today {
        NaiveDate::from_ymd_opt(today.year() + 1, month, day)
    } else {
        Some(this_year)
    }
}

/// The **next** occurrence, strictly after `today`: 1..=7 days ahead, so a
/// weekday named on its own day means a week from now.
///
/// "Due friday" said on a Friday morning is ambiguous, and the strict reading
/// is the one that cannot silently mean "in an hour". Somebody who means today
/// types `today`.
fn next_weekday(today: NaiveDate, days_from_monday: usize) -> NaiveDate {
    let target = days_from_monday as u32 % 7;
    let ahead = (target + 7 - today.weekday().num_days_from_monday()) % 7;
    let ahead = if ahead == 0 { 7 } else { ahead };
    calendar::shift_days(today, i64::from(ahead))
}

/// **`next friday`**: that weekday in the week after this one — Thursday
/// the 5th's *next friday* is the 13th, where its *friday* is the 6th. Weeks
/// start on Monday here, as this parser's weekday numbering does; the
/// unresolved expression ([`DateExpression::NextWeekday`]) leaves the week's
/// start to its resolver.
fn weekday_next_week(today: NaiveDate, days_from_monday: usize) -> NaiveDate {
    let into_week = i64::from(today.weekday().num_days_from_monday());
    let next_monday = calendar::shift_days(today, 7 - into_week);
    calendar::shift_days(next_monday, (days_from_monday % 7) as i64)
}

/// Which day a **bare** time lands on: today, unless that moment has already
/// gone by, in which case tomorrow.
///
/// The comparison is against `now` itself and it is *strict*: a phrase naming
/// the current minute exactly is a due date one minute in the past by the time
/// anybody reads it, so `10:15` typed at 10:15 means tomorrow.
fn today_or_tomorrow(now: NaiveDateTime, at: NaiveTime) -> NaiveDateTime {
    let today = now.date().and_time(at);
    if today > now {
        today
    } else {
        calendar::shift_days(now.date(), 1).and_time(at)
    }
}

/// `9am`, `9:30pm`, `17:30`. Bare digits with no meridiem and no colon are
/// refused — `due 9` is more likely a day than an hour, and guessing wrong
/// commits the wrong kind of value.
///
/// **This is the rule that lets a time stand alone**, and it is the same
/// function the attached form uses: a meridiem suffix or an `HH:MM` colon
/// form, never bare digits. So `31` stays a day-of-month fragment rather than
/// becoming thirty-one o'clock.
fn clock(token: &str, words: &Vocabulary) -> Option<NaiveTime> {
    let (body, meridiem) = words.split_meridiem(token);
    let (h, m): (u32, u32) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse().ok()?, m.parse().ok()?),
        Some(_) => return None,
        None => (body.parse().ok()?, 0),
    };
    if m > 59 {
        return None;
    }
    match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            let h24 = match (h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, false) => h,
                (h, true) => h + 12,
            };
            NaiveTime::from_hms_opt(h24, m, 0)
        }
        // 24-hour needs the colon: `17:30` yes, bare `17` no.
        None if body.contains(':') => NaiveTime::from_hms_opt(h, m, 0),
        None => None,
    }
}

#[cfg(test)]
mod tests;
