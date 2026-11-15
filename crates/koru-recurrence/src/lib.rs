// SPDX-License-Identifier: MPL-2.0

//! Repeating temporal intent, independent of what an occurrence starts.
//!
//! The stored rule is Koru's deliberately bounded five-field cron dialect;
//! the `cron` dependency is an internal parser and explorer. Evaluation is
//! pure: a caller supplies both the reference epoch millisecond and the IANA
//! rules that give a floating rule its current wall-clock meaning. Zoned rules
//! retain their own canonical IANA name.
//!
//! Occurrences have minute precision. [`Occurrence::timing_at`] calls one
//! occurrence due from its exact minute until (but excluding) the next minute;
//! after that it is missed. The evaluator retains no clock or cursor. Moving a
//! supplied reference backward therefore replays the same calculation; the
//! schedule claim keyed by occurrence instant, owned by the scheduling layer,
//! is what prevents a clock rollback from starting the same work twice.

mod dialect;
mod evaluate;

pub use dialect::{
    CronExpression, CronParts, DialectError, MinutePattern, MinuteSet, RuleShape, Weekday,
    WeekdaySet,
};
pub use evaluate::{
    CanonicalZone, EvaluationError, OCCURRENCE_PRECISION_MS, Occurrence, OccurrenceTiming,
    RecurrenceRule, next_zoned_after_at_least,
};

#[cfg(test)]
mod tests;
