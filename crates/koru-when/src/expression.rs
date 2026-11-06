// SPDX-License-Identifier: MPL-2.0

//! Model-free, unresolved temporal expressions.
//!
//! Parsing records what the person said. It does not read a clock, select a
//! timezone, fetch settings, inspect a due date, or name a Koru model type.
//! Those are resolver-edge responsibilities.

use chrono::NaiveTime;

use super::{DateOrder, DurationUnit, Sign, Vocabulary};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    Spanish,
    PortugueseBrazil,
    German,
    French,
    Arabic,
    ChineseSimplified,
    Japanese,
}

impl Language {
    pub const ALL: [Self; 8] = [
        Self::English,
        Self::Spanish,
        Self::PortugueseBrazil,
        Self::German,
        Self::French,
        Self::Arabic,
        Self::ChineseSimplified,
        Self::Japanese,
    ];

    pub const fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
            Self::PortugueseBrazil => "pt-BR",
            Self::German => "de",
            Self::French => "fr",
            Self::Arabic => "ar",
            Self::ChineseSimplified => "zh-Hans",
            Self::Japanese => "ja",
        }
    }

    pub const fn vocabulary(self) -> &'static Vocabulary {
        match self {
            Self::English => &crate::vocab::ENGLISH,
            Self::Spanish => &crate::vocab::SPANISH,
            Self::PortugueseBrazil => &crate::vocab::PORTUGUESE_BRAZIL,
            Self::German => &crate::vocab::GERMAN,
            Self::French => &crate::vocab::FRENCH,
            Self::Arabic => &crate::vocab::ARABIC,
            Self::ChineseSimplified => &crate::vocab::CHINESE_SIMPLIFIED,
            Self::Japanese => &crate::vocab::JAPANESE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemporalExpression {
    Date(DateExpression),
    Wall {
        date: Option<DateExpression>,
        time: TimeExpression,
        /// As typed. Database validation and canonicalization belong to the
        /// resolver edge, not this leaf parser.
        zone: Option<String>,
    },
    Duration(DurationExpression),
    Due(DueExpression),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DateExpression {
    Today,
    Tomorrow,
    Yesterday,
    /// The first day of the next week. The resolver supplies which weekday
    /// starts a week; the parser deliberately reads no device or profile state.
    NextWeek,
    /// Monday = 0, matching chrono's `num_days_from_monday`.
    Weekday(u8),
    /// `next friday`: that weekday in **next week**. Which day starts a week
    /// is the resolver's, as it is for [`NextWeek`](Self::NextWeek).
    NextWeekday(u8),
    MonthDay {
        month: u32,
        day: u32,
        year: Option<i32>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeExpression {
    Clock(NaiveTime),
    Daypart(Daypart),
    EndOfDay,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Daypart {
    Morning,
    Afternoon,
    Evening,
    Tonight,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DurationExpression {
    pub amount: u32,
    pub unit: DurationUnit,
    pub origin: DurationOrigin,
    pub sign: DurationSign,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DurationOrigin {
    Now,
    Selection,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DurationSign {
    Forward,
    Backward,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DueExpression {
    At,
    Before(DurationExpression),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DueRelation {
    At,
    Before,
}

pub fn parse_expression(
    input: &str,
    language: Language,
    order: DateOrder,
) -> Option<TemporalExpression> {
    parse_expression_in(input, order, language.vocabulary())
}

/// Parse the reminder grammar, including reminder-only convenience phrases.
/// The compatibility and due parsers deliberately continue to call
/// [`parse_expression`], so adding reminder vocabulary cannot change `/due`.
pub fn parse_reminder_expression(
    input: &str,
    language: Language,
    order: DateOrder,
) -> Option<TemporalExpression> {
    let words = language.vocabulary();
    let (body, zone) = split_explicit_zone(input);
    let lowered = body.trim().to_lowercase();
    if words.next_week.iter().any(|phrase| lowered == *phrase) {
        return Some(TemporalExpression::Wall {
            date: Some(DateExpression::NextWeek),
            time: TimeExpression::Clock(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            zone,
        });
    }
    parse_expression_in(input, order, words)
}

pub fn parse_expression_in(
    input: &str,
    order: DateOrder,
    words: &Vocabulary,
) -> Option<TemporalExpression> {
    let (body, zone) = split_explicit_zone(input);
    let lowered = body.trim().to_lowercase();
    if lowered.is_empty() {
        return None;
    }

    if words.at_due.iter().any(|phrase| lowered == *phrase) {
        return zone
            .is_none()
            .then_some(TemporalExpression::Due(DueExpression::At));
    }
    for suffix in words.before_due_suffixes {
        if let Some(prefix) = strip_phrase_suffix(&lowered, suffix) {
            let duration = parse_before_due_duration(prefix, words)?;
            return zone
                .is_none()
                .then_some(TemporalExpression::Due(DueExpression::Before(duration)));
        }
    }

    if words.end_of_day.iter().any(|phrase| lowered == *phrase) {
        return Some(TemporalExpression::Wall {
            date: Some(DateExpression::Today),
            time: TimeExpression::EndOfDay,
            zone,
        });
    }
    if words.tonight.iter().any(|phrase| lowered == *phrase) {
        return Some(TemporalExpression::Wall {
            date: None,
            time: TimeExpression::Daypart(Daypart::Tonight),
            zone,
        });
    }

    let prepared = prepare(&lowered, words);
    let joined = super::tokenize(&prepared, words);
    let mut tokens: Vec<&str> = joined.iter().map(String::as_str).collect();
    if tokens.is_empty() {
        return None;
    }

    let trailing_after = tokens
        .last()
        .is_some_and(|token| Vocabulary::is(words.after_word, token));
    if trailing_after {
        tokens.pop();
    }
    if let Some(duration) = parse_duration_tokens(&tokens, words) {
        return zone
            .is_none()
            .then_some(TemporalExpression::Duration(duration));
    }
    if trailing_after {
        return None;
    }

    let (date, used) = parse_date(&tokens, order, words).map_or((None, 0), |(d, n)| (Some(d), n));
    let mut rest = &tokens[used..];
    if rest.is_empty() {
        return match (zone, date) {
            (None, Some(date)) => Some(TemporalExpression::Date(date)),
            _ => None,
        };
    }
    if Vocabulary::is(words.at_word, rest[0]) {
        rest = &rest[1..];
    }
    if rest.len() != 1 {
        return None;
    }
    let time = parse_time(rest[0], words)?;
    Some(TemporalExpression::Wall { date, time, zone })
}

fn parse_before_due_duration(input: &str, words: &Vocabulary) -> Option<DurationExpression> {
    let prepared = prepare(input, words);
    let joined = super::tokenize(&prepared, words);
    let tokens: Vec<&str> = joined.iter().map(String::as_str).collect();
    if tokens
        .first()
        .is_some_and(|token| Vocabulary::is(words.in_word, token))
        || tokens
            .last()
            .is_some_and(|token| Vocabulary::is(words.after_word, token))
    {
        return None;
    }
    let duration = parse_duration_tokens(&tokens, words)?;
    (duration.origin == DurationOrigin::Now && duration.sign == DurationSign::Forward)
        .then_some(duration)
}

fn parse_duration_tokens(tokens: &[&str], words: &Vocabulary) -> Option<DurationExpression> {
    let prefixed = tokens
        .first()
        .is_some_and(|token| Vocabulary::is(words.in_word, token));
    let rest = if prefixed { &tokens[1..] } else { tokens };
    let (amount, unit, sign) = super::duration(rest, words)?;
    if prefixed && sign != Sign::Bare {
        return None;
    }
    let (origin, sign) = match sign {
        Sign::Bare => (DurationOrigin::Now, DurationSign::Forward),
        Sign::Plus => (DurationOrigin::Selection, DurationSign::Forward),
        Sign::Minus => (DurationOrigin::Selection, DurationSign::Backward),
    };
    Some(DurationExpression {
        amount: u32::try_from(amount).ok()?,
        unit,
        origin,
        sign,
    })
}

fn parse_date(
    tokens: &[&str],
    order: DateOrder,
    words: &Vocabulary,
) -> Option<(DateExpression, usize)> {
    let head = *tokens.first()?;
    if Vocabulary::is(words.today, head) {
        return Some((DateExpression::Today, 1));
    }
    if Vocabulary::is(words.tomorrow, head) {
        return Some((DateExpression::Tomorrow, 1));
    }
    if Vocabulary::is(words.yesterday, head) {
        return Some((DateExpression::Yesterday, 1));
    }
    if let Some(index) = tokens.get(1).and_then(|second| words.weekday(second)) {
        let index = u8::try_from(index).ok()?;
        if Vocabulary::is(words.next_word, head) {
            return Some((DateExpression::NextWeekday(index), 2));
        }
        if Vocabulary::is(words.this_word, head) {
            return Some((DateExpression::Weekday(index), 2));
        }
    }
    if let Some(index) = words.weekday(head) {
        return Some((DateExpression::Weekday(u8::try_from(index).ok()?), 1));
    }
    if head.contains(['/', '-']) {
        return parse_numeric_date(head, order).map(|date| (date, 1));
    }
    let (month, day_token) = match (words.month(head), tokens.get(1)) {
        (Some(month), Some(day)) => (month, *day),
        (None, Some(second)) => (words.month(second)?, head),
        _ => return None,
    };
    let day = day_token
        .parse::<u32>()
        .ok()
        .filter(|day| (1..=31).contains(day))?;
    let year = tokens
        .get(2)
        .filter(|token| token.len() == 4)
        .and_then(|token| token.parse::<i32>().ok());
    let used = if year.is_some() { 3 } else { 2 };
    Some((DateExpression::MonthDay { month, day, year }, used))
}

fn parse_numeric_date(token: &str, order: DateOrder) -> Option<DateExpression> {
    let super::NumericDate { year, month, day } = super::numeric_date_parts(token, order)?;
    Some(DateExpression::MonthDay { month, day, year })
}

fn parse_time(token: &str, words: &Vocabulary) -> Option<TimeExpression> {
    if Vocabulary::is(words.morning, token) {
        return Some(TimeExpression::Daypart(Daypart::Morning));
    }
    if Vocabulary::is(words.afternoon, token) {
        return Some(TimeExpression::Daypart(Daypart::Afternoon));
    }
    if Vocabulary::is(words.evening, token) {
        return Some(TimeExpression::Daypart(Daypart::Evening));
    }
    if Vocabulary::is(words.tonight, token) {
        return Some(TimeExpression::Daypart(Daypart::Tonight));
    }
    super::clock(token, words).map(TimeExpression::Clock)
}

fn split_explicit_zone(input: &str) -> (&str, Option<String>) {
    let trimmed = input.trim();
    let Some((body, candidate)) = trimmed.rsplit_once(char::is_whitespace) else {
        return (trimmed, None);
    };
    let abbreviation = (2..=6).contains(&candidate.len())
        && candidate.bytes().all(|byte| byte.is_ascii_uppercase());
    if candidate.contains('/') || abbreviation || matches!(candidate, "UTC" | "GMT") {
        (body.trim_end(), Some(candidate.to_owned()))
    } else {
        (trimmed, None)
    }
}

fn strip_phrase_suffix<'a>(input: &'a str, suffix: &str) -> Option<&'a str> {
    input
        .strip_suffix(suffix)
        .map(str::trim_end)
        .filter(|prefix| !prefix.is_empty())
}

fn prepare(input: &str, words: &Vocabulary) -> String {
    if !words.compact {
        return input.to_owned();
    }
    let value = input.replace('点', ":00");
    let mut terms: Vec<&str> = words
        .today
        .iter()
        .chain(words.tomorrow)
        .chain(words.yesterday)
        .chain(words.morning)
        .chain(words.afternoon)
        .chain(words.evening)
        .chain(words.tonight)
        .chain(words.after_word)
        .chain(words.units.iter().map(|(name, _)| name))
        .copied()
        .collect();
    terms.sort_by_key(|term| std::cmp::Reverse(term.chars().count()));
    terms.dedup();
    let mut segmented = String::with_capacity(value.len() + 8);
    let mut offset = 0;
    while offset < value.len() {
        let rest = &value[offset..];
        if let Some(term) = terms.iter().find(|term| rest.starts_with(**term)) {
            segmented.push(' ');
            segmented.push_str(term);
            segmented.push(' ');
            offset += term.len();
        } else {
            let ch = rest.chars().next().expect("offset is on a char boundary");
            segmented.push(ch);
            offset += ch.len_utf8();
        }
    }
    segmented.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(hour: u32, minute: u32) -> TimeExpression {
        TimeExpression::Clock(NaiveTime::from_hms_opt(hour, minute, 0).unwrap())
    }

    #[test]
    fn equivalent_corpus_phrases_have_identical_unresolved_meaning() {
        let tomorrow_at_nine = [
            (Language::English, "tomorrow at 9am"),
            (Language::Spanish, "mañana 9:00"),
            (Language::PortugueseBrazil, "amanhã 9:00"),
            (Language::German, "morgen 9:00"),
            (Language::French, "demain 9:00"),
            (Language::Arabic, "غدًا 9:00"),
            (Language::ChineseSimplified, "明天9:00"),
            (Language::Japanese, "明日9:00"),
        ];
        let expected = TemporalExpression::Wall {
            date: Some(DateExpression::Tomorrow),
            time: clock(9, 0),
            zone: None,
        };
        for (language, phrase) in tomorrow_at_nine {
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                Some(expected.clone()),
                "{}: {phrase}",
                language.tag()
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
        let expected = TemporalExpression::Duration(DurationExpression {
            amount: 2,
            unit: DurationUnit::Hour,
            origin: DurationOrigin::Now,
            sign: DurationSign::Forward,
        });
        for (language, phrase) in durations {
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                Some(expected.clone()),
                "{}: {phrase}",
                language.tag()
            );
        }

        for language in Language::ALL {
            assert_eq!(
                parse_expression("10/28", language, DateOrder::MonthFirst),
                Some(TemporalExpression::Date(DateExpression::MonthDay {
                    month: 10,
                    day: 28,
                    year: None,
                })),
                "{} numeric date",
                language.tag()
            );
        }
    }

    #[test]
    fn next_week_is_equivalent_in_every_language_and_remains_reminder_only() {
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
        let expected = TemporalExpression::Wall {
            date: Some(DateExpression::NextWeek),
            time: clock(9, 0),
            zone: None,
        };
        for (language, phrase) in phrases {
            assert_eq!(
                parse_reminder_expression(phrase, language, DateOrder::MonthFirst),
                Some(expected.clone()),
                "{}: {phrase}",
                language.tag()
            );
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                None,
                "the shared /due grammar must not gain reminder-only input: {phrase}"
            );
        }
    }

    #[test]
    fn before_due_is_equivalent_in_all_eight_languages() {
        let phrases = [
            (Language::English, "2 hours before due"),
            (Language::Spanish, "2 horas antes del vencimiento"),
            (Language::PortugueseBrazil, "2 horas antes do vencimento"),
            (Language::German, "2 stunden vor fälligkeit"),
            (Language::French, "2 heures avant l'échéance"),
            (Language::Arabic, "2 ساعة قبل الاستحقاق"),
            (Language::ChineseSimplified, "2小时到期前"),
            (Language::Japanese, "2時間期限前"),
        ];
        let expected = TemporalExpression::Due(DueExpression::Before(DurationExpression {
            amount: 2,
            unit: DurationUnit::Hour,
            origin: DurationOrigin::Now,
            sign: DurationSign::Forward,
        }));
        for (language, phrase) in phrases {
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                Some(expected.clone()),
                "{}: {phrase}",
                language.tag()
            );
        }

        for phrase in [
            "-2 hours before due",
            "+2 hours before due",
            "in 2 hours before due",
        ] {
            assert_eq!(
                parse_expression(phrase, Language::English, DateOrder::MonthFirst),
                None,
                "before-due offsets cannot change their reference or direction: {phrase}"
            );
        }
        assert_eq!(
            parse_expression(
                "-2 horas antes del vencimiento",
                Language::Spanish,
                DateOrder::MonthFirst,
            ),
            None
        );
    }

    #[test]
    fn dayparts_end_of_day_and_zone_tokens_remain_semantic() {
        assert_eq!(
            parse_expression("tomorrow morning", Language::English, DateOrder::MonthFirst),
            Some(TemporalExpression::Wall {
                date: Some(DateExpression::Tomorrow),
                time: TimeExpression::Daypart(Daypart::Morning),
                zone: None,
            })
        );
        assert_eq!(
            parse_expression("end of day", Language::English, DateOrder::MonthFirst),
            Some(TemporalExpression::Wall {
                date: Some(DateExpression::Today),
                time: TimeExpression::EndOfDay,
                zone: None,
            })
        );
        for zone in ["America/New_York", "EST"] {
            assert_eq!(
                parse_expression(
                    &format!("tomorrow at 9am {zone}"),
                    Language::English,
                    DateOrder::MonthFirst,
                ),
                Some(TemporalExpression::Wall {
                    date: Some(DateExpression::Tomorrow),
                    time: clock(9, 0),
                    zone: Some(zone.to_owned()),
                })
            );
        }

        let mornings = [
            (Language::English, "tomorrow morning"),
            (Language::Spanish, "mañana mañana"),
            (Language::PortugueseBrazil, "amanhã manhã"),
            (Language::German, "morgen morgens"),
            (Language::French, "demain matin"),
            (Language::Arabic, "غدًا صباحًا"),
            (Language::ChineseSimplified, "明天早上"),
            (Language::Japanese, "明日朝"),
        ];
        for (language, phrase) in mornings {
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                Some(TemporalExpression::Wall {
                    date: Some(DateExpression::Tomorrow),
                    time: TimeExpression::Daypart(Daypart::Morning),
                    zone: None,
                }),
                "{}: {phrase}",
                language.tag()
            );
        }

        let ends = [
            (Language::English, "end of day"),
            (Language::Spanish, "fin del día"),
            (Language::PortugueseBrazil, "fim do dia"),
            (Language::German, "tagesende"),
            (Language::French, "fin de journée"),
            (Language::Arabic, "نهاية اليوم"),
            (Language::ChineseSimplified, "今天结束"),
            (Language::Japanese, "今日の終わり"),
        ];
        for (language, phrase) in ends {
            assert_eq!(
                parse_expression(phrase, language, DateOrder::MonthFirst),
                Some(TemporalExpression::Wall {
                    date: Some(DateExpression::Today),
                    time: TimeExpression::EndOfDay,
                    zone: None,
                }),
                "{}: {phrase}",
                language.tag()
            );
        }
    }

    #[test]
    fn expression_parsing_is_whole_input_or_nothing() {
        for phrase in ["tomorrow 9am extra", "2 hours maybe", "end of day please"] {
            assert_eq!(
                parse_expression(phrase, Language::English, DateOrder::MonthFirst),
                None,
                "{phrase:?}"
            );
        }
        assert_eq!(
            parse_expression("mañana después", Language::Spanish, DateOrder::MonthFirst,),
            None,
            "a localized trailing after marker constrains the duration branch"
        );
    }
}
