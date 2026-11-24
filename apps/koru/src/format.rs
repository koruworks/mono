// SPDX-License-Identifier: MPL-2.0

//! Turning numbers into the words a person reads.
//!
//! Product copy, not a widget — which is why it stays in the app rather than
//! moving to `koru-ui` with the table it feeds. A component should not have an
//! opinion about whether "Yesterday" is capitalised.
//!
//! Rough on purpose. "3 days ago" is what the question wants; a timestamp to the
//! second would be answering a different one.
//!
//! The words themselves live in `koru-i18n` now — this module resolves the
//! app's one language, converts epochs to local wall time, and keeps the
//! call-site signatures the columns have always used.

use chrono::{Datelike, Local, Timelike};
use koru_i18n::Lang;
use koru_i18n::time::DateTimeParts;

/// The app's one display language, resolved once at startup from the OS
/// locale (ADR-87: shown language is the nearest supported one).
///
/// **The global moved down to `koru-i18n`** (ADR-350) and this is the app's
/// name for it. It used to be a private `AtomicU8` here, which meant a leaf
/// surface that draws words — `koru-editor`'s slash menu — could not read
/// the answer this app had already resolved, and English was the only string
/// it could offer. One owner rather than two, and these two functions stay
/// because every call site in this app already spells them.
pub fn set_lang(lang: Lang) {
    koru_i18n::set_current(lang);
}

pub fn lang() -> Lang {
    koru_i18n::current()
}

/// An epoch instant as the reader's local wall clock. `None` for a
/// timestamp outside chrono's range — a state a caller shows as absence.
pub fn local_parts(ms: i64) -> Option<DateTimeParts> {
    let moment = chrono::DateTime::from_timestamp_millis(ms)?.with_timezone(&Local);
    Some(DateTimeParts {
        year: moment.year(),
        month: moment.month() as u8,
        day: moment.day() as u8,
        hour: moment.hour() as u8,
        minute: moment.minute() as u8,
        second: moment.second() as u8,
    })
}

/// An epoch instant as wall-clock fields in explicit, already-sampled zone
/// rules. Maintenance callers use this instead of consulting process-local
/// state while drawing, and tests can inject a device zone without changing
/// the machine running them.
pub fn parts_in_zone(ms: i64, zone: &dyn koru_core::time::ZoneRules) -> Option<DateTimeParts> {
    // `ZoneRules::wall_of` is total for model code, but presentation should
    // preserve the existing absence for a timestamp chrono cannot represent.
    chrono::DateTime::from_timestamp_millis(ms)?;
    let wall = zone.wall_of(ms);
    Some(DateTimeParts {
        year: wall.year(),
        month: wall.month() as u8,
        day: wall.day() as u8,
        hour: wall.hour() as u8,
        minute: wall.minute() as u8,
        second: wall.second() as u8,
    })
}

/// The exact local moment, dated the reader's way, with the year only when
/// it is not this year — what a tooltip over a relative time answers.
pub fn absolute_local(lang: Lang, ms: i64) -> String {
    let Some(parts) = local_parts(ms) else {
        return String::new();
    };
    let include_year = parts.year != Local::now().year();
    koru_i18n::time::absolute(lang, parts, include_year)
}

/// A byte count, as a person reads one.
///
/// **No caller since SP-8b**, which retired the workspace manager's storage
/// column along with the table it lived in. Kept rather than deleted: it is the
/// one place in this app that decides what *4.2 MB* looks like, and the numbers
/// it formatted are a recorded loss rather than a decision that they must never
/// come back.
#[allow(dead_code)]
pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n < K {
        return format!("{n:.0} B");
    }
    for (i, unit) in ["KB", "MB", "GB", "TB"].iter().enumerate() {
        let scaled = n / K.powi(i as i32 + 1);
        if scaled < K || i == 3 {
            return if scaled < 10.0 {
                format!("{scaled:.1} {unit}")
            } else {
                format!("{scaled:.0} {unit}")
            };
        }
    }
    unreachable!()
}

/// How long ago, roughly. What the workspace manager's *Last opened* column
/// said, until SP-8b retired the table — see [`bytes`] for why both outlive
/// their last caller.
///
/// Note there is **no "Today"** here, deliberately: "3 hours ago" is more useful
/// than "today" for something you might have opened twice this morning. The
/// trash wants the opposite emphasis and has its own — see [`deleted_ago`].
pub fn ago(then_ms: i64, now_ms: i64) -> String {
    koru_i18n::time::relative_long(lang(), then_ms, now_ms)
}

/// When something was thrown away, as the trash screen says it.
///
/// **A sibling of [`ago`], not an extension of it.** The trash asks a different
/// question: what matters is roughly how long something has been sitting there,
/// not the hour it went in, so today and yesterday are their own answers and the
/// hours inside today are not worth reporting. Two functions rather than a flag,
/// because the workspace manager's two columns depend on `ago`'s exact wording.
///
/// `None` for an item binned by a build that did not record a time — a state,
/// not an error, so the column simply says so.
pub fn deleted_ago(then_ms: Option<i64>, now_ms: i64) -> String {
    koru_i18n::time::deleted_long(lang(), then_ms, now_ms)
}

/// When the trash removes the item for good, counted down from the 30 days it
/// promises (`koru_core::trash_expiry`), by this device's clock — a notice,
/// not the sweep's decision, which waits a day longer and trusts the Cloud's
/// time. `None` for an item with no recorded time: the sweep keeps it.
pub fn removal_notice(then_ms: Option<i64>, now_ms: i64) -> Option<String> {
    koru_i18n::time::removal_long(
        lang(),
        then_ms.map(|then| koru_core::trash_expiry::days_left(then, now_ms)),
    )
}

/// The trash's "when" cell: how long it has been there and, when known, when
/// it goes — "Deleted 3 days ago · Removed in 27 days".
pub fn trash_when(then_ms: Option<i64>, now_ms: i64) -> String {
    let deleted = deleted_ago(then_ms, now_ms);
    match removal_notice(then_ms, now_ms) {
        Some(removal) => format!("{deleted} · {removal}"),
        None => deleted,
    }
}

/// "3 nested", or nothing at all when there is nothing inside.
///
/// Singular at one, because "1 nested items" is the sort of thing that makes a
/// person distrust the count beside it.
pub fn nested_suffix(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 nested".into()),
        n => Some(format!("{n} nested")),
    }
}

#[cfg(test)]
mod tests;
