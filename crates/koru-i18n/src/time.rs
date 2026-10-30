// SPDX-License-Identifier: MPL-2.0

//! Times, as a person reads them — in their language.
//!
//! Two kinds of answer, deliberately separate:
//!
//! - **Relative** — "5 min", "3 hours ago" — for lists and columns, where
//!   roughly-how-long-ago is the question. The words come from the string
//!   table, so they carry the same completeness guarantee as every other
//!   string; only the bucket arithmetic lives here.
//! - **Absolute** — "08/20 14:32:07" — for the tooltip that answers the
//!   follow-up question exactly. Field *order* is part of a locale
//!   (`dd.MM.` against `MM/dd` against `M月d日`), and order cannot ride in a
//!   sequential-`{}` template, so these are `match` arms per [`Lang`] —
//!   still exhaustive, still a compile error to forget a language.
//!
//! **Epoch→wall-clock conversion is the caller's.** This crate has no
//! dependencies and no clock; the caller converts a timestamp to local
//! [`DateTimeParts`] with whatever time library it already carries, and this
//! module only spells the result. That keeps the leaf a leaf.

use crate::strings::{Key, t};
use crate::{Lang, fill};

/// The clock convention selected by the operating system.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum HourCycle {
    H12,
    H24,
}

/// The field order of a locale's complete numeric date.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NumericDateOrder {
    MonthDayYear,
    DayMonthYear,
    YearMonthDay,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DatePattern {
    MonthDayYear,
    DayMonthYearSlash,
    DayMonthYearDot,
    YearMonthDayHyphen,
    YearMonthDaySlash,
}

#[derive(Copy, Clone)]
struct LocaleConvention {
    tags: &'static [&'static str],
    date: DatePattern,
    clock: HourCycle,
}

/// The explicit locale exceptions Koru supports, transcribed from Unicode
/// CLDR's Gregorian short-date and short-time conventions. A platform clock
/// preference still wins, because people can override their locale default.
const LOCALE_CONVENTIONS: &[LocaleConvention] = &[
    LocaleConvention {
        tags: &["en-us", "en-ph"],
        date: DatePattern::MonthDayYear,
        clock: HourCycle::H12,
    },
    LocaleConvention {
        tags: &["en-ca"],
        date: DatePattern::YearMonthDayHyphen,
        clock: HourCycle::H12,
    },
    LocaleConvention {
        tags: &[
            "en-au", "en-gb", "en-ie", "en-in", "en-nz", "en-sg", "en-za",
        ],
        date: DatePattern::DayMonthYearSlash,
        clock: HourCycle::H24,
    },
    LocaleConvention {
        tags: &["fr-ca"],
        date: DatePattern::YearMonthDayHyphen,
        clock: HourCycle::H24,
    },
    LocaleConvention {
        tags: &["fr-ch"],
        date: DatePattern::DayMonthYearDot,
        clock: HourCycle::H24,
    },
    LocaleConvention {
        tags: &["zh-cn", "zh-sg", "ja-jp"],
        date: DatePattern::YearMonthDaySlash,
        clock: HourCycle::H24,
    },
];

/// Cached locale capabilities for editable and compact date/time values.
///
/// The full BCP-47 tag is kept so regions which share translated strings do
/// not collapse into one date convention. The host supplies the person's OS
/// hour-cycle preference; locale defaults are used only when the platform
/// cannot answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocaleFormat {
    tag: String,
    lang: Lang,
    date: DatePattern,
    clock: HourCycle,
}

impl Default for LocaleFormat {
    fn default() -> Self {
        Self::for_system(Some("en-US"), None)
    }
}

impl LocaleFormat {
    pub fn for_system(tag: Option<&str>, clock: Option<HourCycle>) -> Self {
        let normalized = tag
            .unwrap_or("")
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .replace('_', "-")
            .to_ascii_lowercase();
        let matched = tag.and_then(Lang::matching);
        let lang = matched.unwrap_or(Lang::En);
        let convention_tag = locale_convention_tag(&normalized);
        let convention = LOCALE_CONVENTIONS
            .iter()
            .find(|convention| convention.tags.contains(&convention_tag.as_str()))
            .copied()
            .unwrap_or_else(|| generic_convention(matched));
        Self {
            tag: normalized,
            lang,
            date: convention.date,
            clock: clock.unwrap_or(convention.clock),
        }
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    pub fn hour_cycle(&self) -> HourCycle {
        self.clock
    }

    pub fn date_order(&self) -> NumericDateOrder {
        match self.date {
            DatePattern::MonthDayYear => NumericDateOrder::MonthDayYear,
            DatePattern::DayMonthYearSlash | DatePattern::DayMonthYearDot => {
                NumericDateOrder::DayMonthYear
            }
            DatePattern::YearMonthDayHyphen | DatePattern::YearMonthDaySlash => {
                NumericDateOrder::YearMonthDay
            }
        }
    }

    /// A complete numeric date for an editable field, with the locale's
    /// punctuation, ordering and digits.
    pub fn date_input(&self, parts: DateTimeParts) -> String {
        let ascii = match self.date {
            DatePattern::MonthDayYear => {
                format!("{:02}/{:02}/{:04}", parts.month, parts.day, parts.year)
            }
            DatePattern::DayMonthYearSlash => {
                format!("{:02}/{:02}/{:04}", parts.day, parts.month, parts.year)
            }
            DatePattern::DayMonthYearDot => {
                format!("{:02}.{:02}.{:04}", parts.day, parts.month, parts.year)
            }
            DatePattern::YearMonthDayHyphen => {
                format!("{:04}-{:02}-{:02}", parts.year, parts.month, parts.day)
            }
            DatePattern::YearMonthDaySlash => {
                format!("{:04}/{:02}/{:02}", parts.year, parts.month, parts.day)
            }
        };
        localize_digits(self.lang, &ascii)
    }

    /// A minute-precision clock value for an editable field.
    pub fn time_input(&self, parts: DateTimeParts) -> String {
        let ascii = match self.clock {
            HourCycle::H24 => format!("{:02}:{:02}", parts.hour, parts.minute),
            HourCycle::H12 => {
                let hour = match parts.hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                let suffix = if parts.hour < 12 {
                    if self.lang == Lang::Ar {
                        "\u{0635}"
                    } else {
                        "AM"
                    }
                } else if self.lang == Lang::Ar {
                    "\u{0645}"
                } else {
                    "PM"
                };
                format!("{hour}:{:02} {suffix}", parts.minute)
            }
        };
        localize_digits(self.lang, &ascii)
    }

    pub fn date_time(&self, parts: DateTimeParts) -> String {
        format!("{} {}", self.date_input(parts), self.time_input(parts))
    }

    pub fn parse_date_input(&self, value: &str) -> Option<(i32, u32, u32)> {
        let ascii = ascii_digits(value);
        let numbers = ascii
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let [a, b, c] = numbers.as_slice() else {
            return None;
        };
        Some(match self.date {
            DatePattern::MonthDayYear => (*c as i32, *a, *b),
            DatePattern::DayMonthYearSlash | DatePattern::DayMonthYearDot => (*c as i32, *b, *a),
            DatePattern::YearMonthDayHyphen | DatePattern::YearMonthDaySlash => (*a as i32, *b, *c),
        })
    }

    pub fn parse_time_input(&self, value: &str) -> Option<(u32, u32)> {
        let ascii = ascii_digits(value);
        let upper = ascii.trim().to_ascii_uppercase();
        let (clock, meridiem) = upper
            .strip_suffix(" AM")
            .map(|clock| (clock, Some(false)))
            .or_else(|| upper.strip_suffix(" PM").map(|clock| (clock, Some(true))))
            .or_else(|| {
                ascii
                    .trim()
                    .strip_suffix(" \u{0635}")
                    .map(|clock| (clock, Some(false)))
            })
            .or_else(|| {
                ascii
                    .trim()
                    .strip_suffix(" \u{0645}")
                    .map(|clock| (clock, Some(true)))
            })
            .unwrap_or((upper.as_str(), None));
        let (hour, minute) = clock.split_once(':')?;
        let mut hour = hour.trim().parse::<u32>().ok()?;
        let minute = minute.trim().parse::<u32>().ok()?;
        if let Some(pm) = meridiem {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + u32::from(pm) * 12;
        }
        (hour < 24 && minute < 60).then_some((hour, minute))
    }

    /// An ungrouped whole number for a compact editable field.
    ///
    /// Unlike [`crate::number::decimal`], input fields do not add thousands
    /// separators: their punctuation belongs to the field itself. Digits are
    /// still the locale's digits, including Arabic-Indic digits for Arabic.
    pub fn whole_number_input(&self, value: u32) -> String {
        localize_digits(self.lang, &value.to_string())
    }

    /// Read an ungrouped whole number entered with either ASCII or the
    /// locale's decimal digits.
    pub fn parse_whole_number_input(&self, value: &str) -> Option<u32> {
        ascii_digits(value.trim()).parse().ok()
    }
}

/// Reduce a full BCP-47 tag to the language/region key used by CLDR's
/// regional exceptions. Scripts, variants and Unicode extensions remain in
/// `LocaleFormat::tag`; they simply do not hide an explicit region here.
fn locale_convention_tag(tag: &str) -> String {
    let mut parts = tag.split('-');
    let language = parts.next().unwrap_or("");
    let region = parts.find(|part| {
        (part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()))
            || (part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_digit()))
    });
    region.map_or_else(
        || language.to_owned(),
        |region| format!("{language}-{region}"),
    )
}

fn generic_convention(lang: Option<Lang>) -> LocaleConvention {
    let (date, clock) = match lang {
        None | Some(Lang::En) => (DatePattern::MonthDayYear, HourCycle::H12),
        Some(Lang::De) => (DatePattern::DayMonthYearDot, HourCycle::H24),
        Some(Lang::ZhHans | Lang::Ja) => (DatePattern::YearMonthDaySlash, HourCycle::H24),
        Some(Lang::Es | Lang::PtBr | Lang::Fr) => (DatePattern::DayMonthYearSlash, HourCycle::H24),
        Some(Lang::Ar) => (DatePattern::DayMonthYearSlash, HourCycle::H12),
    };
    LocaleConvention {
        tags: &[],
        date,
        clock,
    }
}

fn localize_digits(lang: Lang, value: &str) -> String {
    if lang != Lang::Ar {
        return value.to_owned();
    }
    const DIGITS: [char; 10] = [
        '\u{0660}', '\u{0661}', '\u{0662}', '\u{0663}', '\u{0664}', '\u{0665}', '\u{0666}',
        '\u{0667}', '\u{0668}', '\u{0669}',
    ];
    value
        .chars()
        .map(|ch| {
            ch.to_digit(10)
                .map(|digit| DIGITS[digit as usize])
                .unwrap_or(ch)
        })
        .collect()
}

fn ascii_digits(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            let code = ch as u32;
            let digit = if (0x0660..=0x0669).contains(&code) {
                Some(code - 0x0660)
            } else if (0x06f0..=0x06f9).contains(&code) {
                Some(code - 0x06f0)
            } else {
                None
            };
            digit
                .and_then(|digit| char::from_digit(digit, 10))
                .unwrap_or(ch)
        })
        .collect()
}

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const MONTH: i64 = 30 * DAY;

/// Compact relative time for a dense row: "now", "5 min", "1h", "3d", "2m".
///
/// No "ago" word, no future tense — the column it sits in says which way
/// time runs. Months are the top bucket, as in [`relative_long`]: "14m" is
/// still a better answer than a year count for something a person is
/// scanning a history for.
pub fn relative_narrow(lang: Lang, then_ms: i64, now_ms: i64) -> String {
    let secs = ((now_ms - then_ms) / 1000).max(0);
    match secs {
        s if s < MINUTE => t(lang, Key::TimeNow).to_string(),
        s if s < HOUR => fill(
            t(lang, Key::TimeMinutesNarrow),
            &[&(s / MINUTE).to_string()],
        ),
        s if s < DAY => fill(t(lang, Key::TimeHoursNarrow), &[&(s / HOUR).to_string()]),
        // Days run through "30d": the month bucket starts only once a full
        // 31st day has passed, so the last day-count a person sees is 30,
        // never a 29 that quietly skips to "1m".
        s if s < MONTH + DAY => fill(t(lang, Key::TimeDaysNarrow), &[&(s / DAY).to_string()]),
        s => fill(t(lang, Key::TimeMonthsNarrow), &[&(s / MONTH).to_string()]),
    }
}

/// How long ago, roughly, as a sentence fragment: "Just now", "3 hours ago",
/// "Yesterday". The localized successor of the app's English-only `ago`,
/// bucket for bucket — including the deliberate absence of "Today", which
/// that function's doc comment argues for.
pub fn relative_long(lang: Lang, then_ms: i64, now_ms: i64) -> String {
    let secs = ((now_ms - then_ms) / 1000).max(0);
    match secs {
        s if s < MINUTE => t(lang, Key::TimeJustNow).to_string(),
        s if s < HOUR => fill(t(lang, Key::TimeMinutesAgo), &[&(s / MINUTE).to_string()]),
        s if s < DAY => fill(t(lang, Key::TimeHoursAgo), &[&(s / HOUR).to_string()]),
        s if s < 2 * DAY => t(lang, Key::TimeYesterday).to_string(),
        s if s < MONTH => fill(t(lang, Key::TimeDaysAgo), &[&(s / DAY).to_string()]),
        s => fill(t(lang, Key::TimeMonthsAgo), &[&(s / MONTH).to_string()]),
    }
}

/// When something was thrown away, as the trash says it. A sibling of
/// [`relative_long`] with its own buckets — the trash's question is how long
/// something has been sitting there, so today and yesterday are their own
/// answers and the hours inside today are not worth reporting.
///
/// `None` for an item binned by a build that did not record a time — a
/// state, not an error, so the column simply says so.
pub fn deleted_long(lang: Lang, then_ms: Option<i64>, now_ms: i64) -> String {
    let Some(then) = then_ms else {
        return t(lang, Key::DeletedUnknown).to_string();
    };
    let secs = ((now_ms - then) / 1000).max(0);
    match secs {
        s if s < DAY => t(lang, Key::DeletedToday).to_string(),
        s if s < 2 * DAY => t(lang, Key::DeletedYesterday).to_string(),
        s if s < MONTH => fill(t(lang, Key::DeletedDaysAgo), &[&(s / DAY).to_string()]),
        s if s < 2 * MONTH => t(lang, Key::DeletedLastMonth).to_string(),
        s => fill(t(lang, Key::DeletedMonthsAgo), &[&(s / MONTH).to_string()]),
    }
}

/// A wall-clock moment, already converted to the reader's local time zone.
/// The caller owns that conversion; see the module note.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DateTimeParts {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// The exact moment, in the reader's date order: "08/20 14:32:07" in
/// English, "20.08. 14:32:07" in German, "8月20日 14:32:07" in Japanese.
///
/// `include_year` is the caller's call — a tooltip includes it when the
/// moment's year is not the current one, a log line always might.
pub fn absolute(lang: Lang, parts: DateTimeParts, include_year: bool) -> String {
    let DateTimeParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
    } = parts;
    let time = format!("{hour:02}:{minute:02}:{second:02}");
    match lang {
        Lang::En => {
            if include_year {
                format!("{month:02}/{day:02}/{year} {time}")
            } else {
                format!("{month:02}/{day:02} {time}")
            }
        }
        Lang::Es | Lang::PtBr | Lang::Fr | Lang::Ar => {
            if include_year {
                format!("{day:02}/{month:02}/{year} {time}")
            } else {
                format!("{day:02}/{month:02} {time}")
            }
        }
        Lang::De => {
            if include_year {
                format!("{day:02}.{month:02}.{year} {time}")
            } else {
                format!("{day:02}.{month:02}. {time}")
            }
        }
        Lang::ZhHans | Lang::Ja => {
            if include_year {
                format!("{year}年{month}月{day}日 {time}")
            } else {
                format!("{month}月{day}日 {time}")
            }
        }
    }
}

/// The date alone, in the reader's order: "08/20/2026", "20.08.2026",
/// "2026年8月20日". The sibling of [`absolute`] for labels that answer
/// *which day* without the time of it.
pub fn absolute_date(lang: Lang, parts: DateTimeParts, include_year: bool) -> String {
    let DateTimeParts {
        year, month, day, ..
    } = parts;
    match lang {
        Lang::En => {
            if include_year {
                format!("{month:02}/{day:02}/{year}")
            } else {
                format!("{month:02}/{day:02}")
            }
        }
        Lang::Es | Lang::PtBr | Lang::Fr | Lang::Ar => {
            if include_year {
                format!("{day:02}/{month:02}/{year}")
            } else {
                format!("{day:02}/{month:02}")
            }
        }
        Lang::De => {
            if include_year {
                format!("{day:02}.{month:02}.{year}")
            } else {
                format!("{day:02}.{month:02}.")
            }
        }
        Lang::ZhHans | Lang::Ja => {
            if include_year {
                format!("{year}年{month}月{day}日")
            } else {
                format!("{month}月{day}日")
            }
        }
    }
}

/// **The day with its name**: "Thursday 22 October", "jeudi 22 octobre",
/// "22. Oktober, Donnerstag"-style orders per language, "10月22日 星期四".
/// What a page that *is* today prints at its head (workspace foundation,
/// Home's Today page); the numeric [`absolute_date`] stays for labels that
/// only answer which day. No year: the page is about today.
///
/// The weekday is computed from the parts (Zeller, proleptic Gregorian) so
/// the caller passes the same `DateTimeParts` every other formatter takes.
pub fn date_long(lang: Lang, parts: DateTimeParts) -> String {
    let DateTimeParts {
        year, month, day, ..
    } = parts;
    let weekday = weekday_of(year, month, day);
    let m = usize::from(month.clamp(1, 12)) - 1;
    let text = match lang {
        Lang::En => format!("{} {day} {}", WEEKDAYS_EN[weekday], MONTHS_EN[m]),
        Lang::Es => format!("{} {day} de {}", WEEKDAYS_ES[weekday], MONTHS_ES[m]),
        Lang::PtBr => format!("{}, {day} de {}", WEEKDAYS_PT_BR[weekday], MONTHS_PT_BR[m]),
        Lang::De => format!("{}, {day}. {}", WEEKDAYS_DE[weekday], MONTHS_DE[m]),
        Lang::Fr => format!("{} {day} {}", WEEKDAYS_FR[weekday], MONTHS_FR[m]),
        Lang::Ar => format!("{} {day} {}", WEEKDAYS_AR[weekday], MONTHS_AR[m]),
        Lang::ZhHans => format!("{month}月{day}日 {}", WEEKDAYS_ZH_HANS[weekday]),
        Lang::Ja => format!("{month}月{day}日（{}）", WEEKDAYS_JA[weekday]),
    };
    localize_digits(lang, &text)
}

/// 0 = Monday … 6 = Sunday, from a proleptic Gregorian date.
fn weekday_of(year: i32, month: u8, day: u8) -> usize {
    // Zeller's congruence, with January and February as months 13 and 14
    // of the year before.
    let (y, m) = if month < 3 {
        (year - 1, i32::from(month) + 12)
    } else {
        (year, i32::from(month))
    };
    let d = i32::from(day);
    let k = y.rem_euclid(100);
    let j = y.div_euclid(100);
    let h = (d + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j).rem_euclid(7);
    // Zeller: 0 = Saturday … 6 = Friday → Monday-first.
    ((h + 5) % 7) as usize
}

const WEEKDAYS_EN: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS_ES: [&str; 7] = [
    "lunes",
    "martes",
    "miércoles",
    "jueves",
    "viernes",
    "sábado",
    "domingo",
];
const MONTHS_ES: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];
const WEEKDAYS_PT_BR: [&str; 7] = [
    "segunda-feira",
    "terça-feira",
    "quarta-feira",
    "quinta-feira",
    "sexta-feira",
    "sábado",
    "domingo",
];
const MONTHS_PT_BR: [&str; 12] = [
    "janeiro",
    "fevereiro",
    "março",
    "abril",
    "maio",
    "junho",
    "julho",
    "agosto",
    "setembro",
    "outubro",
    "novembro",
    "dezembro",
];
const WEEKDAYS_DE: [&str; 7] = [
    "Montag",
    "Dienstag",
    "Mittwoch",
    "Donnerstag",
    "Freitag",
    "Samstag",
    "Sonntag",
];
const MONTHS_DE: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];
const WEEKDAYS_FR: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MONTHS_FR: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const WEEKDAYS_AR: [&str; 7] = [
    "الاثنين",
    "الثلاثاء",
    "الأربعاء",
    "الخميس",
    "الجمعة",
    "السبت",
    "الأحد",
];
const MONTHS_AR: [&str; 12] = [
    "يناير",
    "فبراير",
    "مارس",
    "أبريل",
    "مايو",
    "يونيو",
    "يوليو",
    "أغسطس",
    "سبتمبر",
    "أكتوبر",
    "نوفمبر",
    "ديسمبر",
];
const WEEKDAYS_ZH_HANS: [&str; 7] = [
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
    "星期日",
];
const WEEKDAYS_JA: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1000;

    /// Eight fixtures for one day — 22 October 2026, a Thursday — in each
    /// language's own order and words; the weekday falls out of the parts.
    #[test]
    fn the_long_date_names_the_day_in_every_language() {
        let day = DateTimeParts {
            year: 2026,
            month: 10,
            day: 22,
            hour: 9,
            minute: 0,
            second: 0,
        };
        let want = [
            (Lang::En, "Thursday 22 October"),
            (Lang::Es, "jueves 22 de octubre"),
            (Lang::PtBr, "quinta-feira, 22 de outubro"),
            (Lang::De, "Donnerstag, 22. Oktober"),
            (Lang::Fr, "jeudi 22 octobre"),
            (Lang::Ar, "الخميس ٢٢ أكتوبر"),
            (Lang::ZhHans, "10月22日 星期四"),
            (Lang::Ja, "10月22日（木）"),
        ];
        for (lang, text) in want {
            assert_eq!(date_long(lang, day), text, "{lang:?}");
        }
        // The calendar's corners: a leap day, a New Year, a year's end.
        assert_eq!(weekday_of(2024, 2, 29), 3, "29 Feb 2024 was a Thursday");
        assert_eq!(weekday_of(2026, 1, 1), 3, "1 Jan 2026 is a Thursday");
        assert_eq!(weekday_of(2000, 12, 31), 6, "31 Dec 2000 was a Sunday");
        assert_eq!(weekday_of(2025, 10, 20), 0, "20 Oct 2025 was a Monday");
    }

    #[test]
    fn narrow_buckets_match_the_spec() {
        let now = 100 * MONTH * MS;
        let at = |secs_ago: i64| relative_narrow(Lang::En, now - secs_ago * MS, now);
        assert_eq!(at(30), "now");
        assert_eq!(at(MINUTE), "1 min");
        assert_eq!(at(5 * MINUTE), "5 min");
        assert_eq!(at(HOUR), "1h");
        assert_eq!(at(DAY), "1d");
        assert_eq!(at(2 * DAY), "2d");
        assert_eq!(at(30 * DAY), "30d");
        assert_eq!(at(31 * DAY), "1m");
        assert_eq!(at(14 * MONTH), "14m");
    }

    /// The long English forms are the exact wording `apps/koru`'s columns
    /// have always shown; its `ago_is_unchanged` test depends on them.
    #[test]
    fn long_english_wording_is_unchanged() {
        let now = 100 * MONTH * MS;
        let at = |secs_ago: i64| relative_long(Lang::En, now - secs_ago * MS, now);
        assert_eq!(at(10), "Just now");
        assert_eq!(at(5 * MINUTE), "5 minutes ago");
        assert_eq!(at(3 * HOUR), "3 hours ago");
        assert_eq!(at(DAY), "Yesterday");
        assert_eq!(at(3 * DAY), "3 days ago");
        assert_eq!(at(2 * MONTH), "2 months ago");
    }

    #[test]
    fn deleted_english_wording_is_unchanged() {
        let now = 100 * MONTH * MS;
        let at = |secs_ago: i64| deleted_long(Lang::En, Some(now - secs_ago * MS), now);
        assert_eq!(deleted_long(Lang::En, None, now), "Deleted");
        assert_eq!(at(HOUR), "Deleted today");
        assert_eq!(at(DAY), "Deleted yesterday");
        assert_eq!(at(3 * DAY), "Deleted 3 days ago");
        assert_eq!(at(MONTH), "Deleted last month");
        assert_eq!(at(3 * MONTH), "Deleted 3 months ago");
    }

    #[test]
    fn a_future_timestamp_reads_as_now_not_as_negative_time() {
        let now = MONTH * MS;
        assert_eq!(relative_narrow(Lang::En, now + DAY * MS, now), "now");
        assert_eq!(relative_long(Lang::En, now + DAY * MS, now), "Just now");
    }

    #[test]
    fn every_language_orders_the_date_its_own_way() {
        let parts = DateTimeParts {
            year: 2026,
            month: 8,
            day: 20,
            hour: 14,
            minute: 32,
            second: 7,
        };
        assert_eq!(absolute(Lang::En, parts, false), "08/20 14:32:07");
        assert_eq!(absolute(Lang::En, parts, true), "08/20/2026 14:32:07");
        assert_eq!(absolute(Lang::De, parts, false), "20.08. 14:32:07");
        assert_eq!(absolute(Lang::De, parts, true), "20.08.2026 14:32:07");
        assert_eq!(absolute(Lang::Fr, parts, false), "20/08 14:32:07");
        assert_eq!(absolute(Lang::Ja, parts, false), "8月20日 14:32:07");
        assert_eq!(absolute(Lang::Ja, parts, true), "2026年8月20日 14:32:07");
        // Every language answers; the match is exhaustive, but the loop says
        // nothing panics and nothing is empty.
        for lang in Lang::ALL {
            assert!(!absolute(lang, parts, true).is_empty());
            assert!(!absolute_date(lang, parts, true).is_empty());
        }
    }

    #[test]
    fn the_date_alone_keeps_the_same_order() {
        let parts = DateTimeParts {
            year: 2026,
            month: 8,
            day: 20,
            hour: 14,
            minute: 32,
            second: 7,
        };
        assert_eq!(absolute_date(Lang::En, parts, true), "08/20/2026");
        assert_eq!(absolute_date(Lang::De, parts, true), "20.08.2026");
        assert_eq!(absolute_date(Lang::Ja, parts, true), "2026年8月20日");
        assert_eq!(absolute_date(Lang::Es, parts, false), "20/08");
    }

    #[test]
    fn full_locale_keeps_region_clock_punctuation_and_digits() {
        let parts = DateTimeParts {
            year: 2026,
            month: 9,
            day: 4,
            hour: 21,
            minute: 7,
            second: 0,
        };
        for tag in ["en-GB", "en-AU"] {
            let locale = LocaleFormat::for_system(Some(tag), None);
            assert_eq!(locale.date_time(parts), "04/09/2026 21:07", "{tag}");
        }
        let en_ca = LocaleFormat::for_system(Some("en-CA"), None);
        assert_eq!(en_ca.date_time(parts), "2026-09-04 9:07 PM");
        assert_eq!(en_ca.date_order(), NumericDateOrder::YearMonthDay);
        let fr_ca = LocaleFormat::for_system(Some("fr-CA"), None);
        assert_eq!(fr_ca.date_time(parts), "2026-09-04 21:07");
        assert_eq!(fr_ca.date_order(), NumericDateOrder::YearMonthDay);
        assert_eq!(
            LocaleFormat::for_system(Some("en-Latn-CA-u-hc-h23"), None).date_input(parts),
            "2026-09-04"
        );
        let german = LocaleFormat::for_system(Some("de-DE"), None);
        assert_eq!(german.date_time(parts), "04.09.2026 21:07");
        assert_eq!(
            LocaleFormat::for_system(Some("fr-CH"), None).date_input(parts),
            "04.09.2026"
        );
        assert_eq!(
            LocaleFormat::for_system(Some("zh-CN"), None).date_input(parts),
            "2026/09/04"
        );
        assert_eq!(
            LocaleFormat::for_system(Some("es-MX"), None).date_input(parts),
            "04/09/2026"
        );

        let arabic = LocaleFormat::for_system(Some("ar-EG"), Some(HourCycle::H24));
        assert_eq!(
            arabic.date_time(parts),
            "\u{0660}\u{0664}/\u{0660}\u{0669}/\u{0662}\u{0660}\u{0662}\u{0666} \u{0662}\u{0661}:\u{0660}\u{0667}"
        );
        assert_eq!(
            arabic.parse_date_input(
                "\u{0660}\u{0664}/\u{0660}\u{0669}/\u{0662}\u{0660}\u{0662}\u{0666}"
            ),
            Some((2026, 9, 4))
        );
        assert_eq!(
            arabic.parse_time_input("\u{0662}\u{0661}:\u{0660}\u{0667}"),
            Some((21, 7))
        );
    }

    #[test]
    fn compact_whole_number_input_round_trips_localized_digits() {
        for lang in crate::Lang::ALL {
            let locale = LocaleFormat::for_system(Some(lang.tag()), None);
            let text = locale.whole_number_input(31);
            assert_eq!(locale.parse_whole_number_input(&text), Some(31), "{lang:?}");
        }
        let ar = LocaleFormat::for_system(Some("ar"), None);
        assert_eq!(ar.whole_number_input(31), "٣١");
        assert_eq!(ar.parse_whole_number_input("۳۱"), Some(31));
        assert_eq!(ar.parse_whole_number_input("٣٢ days"), None);
    }

    #[test]
    fn platform_clock_preference_overrides_locale_default() {
        let parts = DateTimeParts {
            year: 2026,
            month: 9,
            day: 4,
            hour: 21,
            minute: 7,
            second: 0,
        };
        let us_24 = LocaleFormat::for_system(Some("en-US"), Some(HourCycle::H24));
        let gb_12 = LocaleFormat::for_system(Some("en-GB"), Some(HourCycle::H12));
        assert_eq!(us_24.time_input(parts), "21:07");
        assert_eq!(gb_12.time_input(parts), "9:07 PM");
    }
}
