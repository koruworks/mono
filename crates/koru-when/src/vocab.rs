// SPDX-License-Identifier: MPL-2.0

//! **Every word the grammar knows is in one table.**
//!
//! This is the localization seam. It holds the phase-one vocabulary for all
//! eight Koru languages: English, Spanish, Brazilian Portuguese, German,
//! French, Arabic, Simplified Chinese, and Japanese. A grammar which spells its
//! words inside `match` arms cannot add or review a language independently —
//! `"friday" | "fri" =>` is a sentence about English hiding in control flow.
//!
//! **Nothing below is a `match` on a literal.** The parser asks a
//! [`Vocabulary`] for a month, a weekday, a unit or a keyword, and a second
//! language is a second `const` beside [`ENGLISH`] selected by
//! [`crate::Language`]. English keeps prefix abbreviations for legacy parity;
//! the other languages accept only their reviewed full spellings.
//!
//! ## How a name is matched
//!
//! Each month and weekday is a **list of spellings**, and a token matches when
//! it is a prefix of any of them, at least [`Vocabulary::min_abbrev`]
//! characters long. That is the whole abbreviation rule, and it is why `sep`,
//! `sept`, `septem` and `september` all parse without any of them being
//! written down. It replaces a special case: the editor's parser carried a
//! literal `if token == "sept" { "sep" }` because September is the one English
//! month whose common short form is four letters, which is a rule about
//! English typography expressed as a rewrite of one string.
//!
//! A language whose abbreviation is *not* a prefix of the full name lists it as
//! a second spelling rather than needing new code.
//!
//! **Ambiguity is the thing to check when adding a language.** With three
//! letters, English has no collision — `jun`/`jul`, `mar`/`may`,
//! `tue`/`thu`/`sat`/`sun` all separate — and
//! `every_month_prefix_resolves_to_its_own_month` proves it rather than
//! assuming it. A language where two months share their first three letters
//! raises `min_abbrev`, or lists the disambiguated spellings.

/// The five units a duration phrase can name.
///
/// **The split at `Day` is semantic, not arithmetic**: minutes and hours land
/// on a wall clock, days and longer land on a calendar day. "In three days" is
/// a day, not 10:15 three days from now.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DurationUnit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}

impl DurationUnit {
    /// Whether this unit answers a calendar day rather than a wall clock.
    pub fn is_calendar(self) -> bool {
        !matches!(self, DurationUnit::Minute | DurationUnit::Hour)
    }
}

/// One language's words. `'static` throughout — a vocabulary is a constant, not
/// something loaded, because the parser has no I/O and never will.
pub struct Vocabulary {
    /// BCP-47-ish tag, for a caller that has to choose one. Never parsed.
    pub tag: &'static str,
    /// Months, January first. Each entry is that month's spellings; the first
    /// is canonical and the rest exist for names whose short form is not a
    /// prefix of the long one.
    pub months: [&'static [&'static str]; 12],
    /// Weekdays, **Monday first** — the order `chrono::Weekday`'s
    /// `num_days_from_monday` counts in, so an index here is that number.
    pub weekdays: [&'static [&'static str]; 7],
    /// Shortest accepted abbreviation, in characters. Below this a token is
    /// not offered to the name tables at all, which is what stops `ju` from
    /// having to choose between June and July.
    pub min_abbrev: usize,
    /// Whether month/weekday spellings accept prefixes. English does; the
    /// other seven tables match whole names so a short translated fragment is
    /// never guessed.
    pub prefix_names: bool,
    /// Today, tomorrow, yesterday — matched whole, never by prefix.
    pub today: &'static [&'static str],
    pub tomorrow: &'static [&'static str],
    pub yesterday: &'static [&'static str],
    /// The preposition that opens a duration counted **from now**: `in 2w`.
    pub in_word: &'static [&'static str],
    /// The optional word between a day and a time: `friday at 5pm`.
    pub at_word: &'static [&'static str],
    /// Morning and afternoon meridiems. Matched whole when they stand alone
    /// (so the tokenizer can glue `5 pm`) and as a suffix inside a clock token
    /// (`5pm`).
    pub am: &'static [&'static str],
    pub pm: &'static [&'static str],
    /// Duration units, matched **whole** — a prefix rule here would make `m`
    /// ambiguous between minutes and months.
    pub units: &'static [(&'static str, DurationUnit)],
    /// Daypart words. These produce semantic enum values, never display text.
    pub morning: &'static [&'static str],
    pub afternoon: &'static [&'static str],
    pub evening: &'static [&'static str],
    pub tonight: &'static [&'static str],
    /// **A modifier on a weekday**: `next friday` is Friday of next week,
    /// `this friday` the coming one — the bare weekday's own reading. One
    /// rule over every weekday the table names, not a phrase list. Empty
    /// where the language puts the word elsewhere (`vendredi prochain`),
    /// until that order is written as its own rule.
    pub next_word: &'static [&'static str],
    pub this_word: &'static [&'static str],
    /// Whole localized phrases and due-relative suffixes used by the
    /// unresolved expression parser.
    pub next_week: &'static [&'static str],
    pub end_of_day: &'static [&'static str],
    pub at_due: &'static [&'static str],
    pub before_due_suffixes: &'static [&'static str],
    /// A trailing duration marker (`2 horas depois`, `2小时后`).
    pub after_word: &'static [&'static str],
    /// For CJK input without spaces, known vocabulary is segmented before the
    /// grammar runs. No general tokenizer or locale-specific collation enters
    /// canonical state.
    pub compact: bool,
}

/// English words, used by both the compatibility parser and unresolved API.
pub const ENGLISH: Vocabulary = Vocabulary {
    tag: "en",
    months: [
        &["january"],
        &["february"],
        &["march"],
        &["april"],
        &["may"],
        &["june"],
        &["july"],
        &["august"],
        &["september"],
        &["october"],
        &["november"],
        &["december"],
    ],
    weekdays: [
        &["monday"],
        &["tuesday"],
        &["wednesday"],
        &["thursday"],
        &["friday"],
        &["saturday"],
        &["sunday"],
    ],
    min_abbrev: 3,
    prefix_names: true,
    today: &["today"],
    tomorrow: &["tomorrow"],
    yesterday: &["yesterday"],
    in_word: &["in"],
    at_word: &["at"],
    am: &["am"],
    pm: &["pm"],
    units: &[
        ("m", DurationUnit::Minute),
        ("min", DurationUnit::Minute),
        ("mins", DurationUnit::Minute),
        ("minute", DurationUnit::Minute),
        ("minutes", DurationUnit::Minute),
        ("h", DurationUnit::Hour),
        ("hr", DurationUnit::Hour),
        ("hrs", DurationUnit::Hour),
        ("hour", DurationUnit::Hour),
        ("hours", DurationUnit::Hour),
        ("d", DurationUnit::Day),
        ("day", DurationUnit::Day),
        ("days", DurationUnit::Day),
        ("w", DurationUnit::Week),
        ("wk", DurationUnit::Week),
        ("wks", DurationUnit::Week),
        ("week", DurationUnit::Week),
        ("weeks", DurationUnit::Week),
        ("mo", DurationUnit::Month),
        ("mos", DurationUnit::Month),
        ("month", DurationUnit::Month),
        ("months", DurationUnit::Month),
    ],
    morning: &["morning"],
    afternoon: &["afternoon"],
    evening: &["evening"],
    tonight: &["tonight"],
    next_word: &["next"],
    this_word: &["this", "coming"],
    next_week: &["next week"],
    end_of_day: &["end of day", "eod"],
    at_due: &["at due", "due"],
    before_due_suffixes: &["before due"],
    after_word: &[],
    compact: false,
};

pub const SPANISH: Vocabulary = Vocabulary {
    tag: "es",
    months: [
        &["enero"],
        &["febrero"],
        &["marzo"],
        &["abril"],
        &["mayo"],
        &["junio"],
        &["julio"],
        &["agosto"],
        &["septiembre"],
        &["octubre"],
        &["noviembre"],
        &["diciembre"],
    ],
    weekdays: [
        &["lunes"],
        &["martes"],
        &["miércoles", "miercoles"],
        &["jueves"],
        &["viernes"],
        &["sábado", "sabado"],
        &["domingo"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["hoy"],
    tomorrow: &["mañana", "manana"],
    yesterday: &["ayer"],
    in_word: &["en", "dentro"],
    at_word: &["a", "alas"],
    am: &["am"],
    pm: &["pm"],
    units: &[
        ("m", DurationUnit::Minute),
        ("min", DurationUnit::Minute),
        ("minuto", DurationUnit::Minute),
        ("minutos", DurationUnit::Minute),
        ("h", DurationUnit::Hour),
        ("hora", DurationUnit::Hour),
        ("horas", DurationUnit::Hour),
        ("d", DurationUnit::Day),
        ("día", DurationUnit::Day),
        ("dias", DurationUnit::Day),
        ("días", DurationUnit::Day),
        ("s", DurationUnit::Week),
        ("semana", DurationUnit::Week),
        ("semanas", DurationUnit::Week),
        ("mes", DurationUnit::Month),
        ("meses", DurationUnit::Month),
    ],
    morning: &["mañana", "manana"],
    afternoon: &["tarde"],
    evening: &["noche"],
    tonight: &["esta noche"],
    next_word: &[],
    this_word: &[],
    next_week: &[
        "la próxima semana",
        "la proxima semana",
        "próxima semana",
        "proxima semana",
    ],
    end_of_day: &["fin del día", "fin del dia"],
    at_due: &["al vencimiento", "en vencimiento"],
    before_due_suffixes: &["antes del vencimiento"],
    after_word: &["después", "despues"],
    compact: false,
};

pub const PORTUGUESE_BRAZIL: Vocabulary = Vocabulary {
    tag: "pt-BR",
    months: [
        &["janeiro"],
        &["fevereiro"],
        &["março", "marco"],
        &["abril"],
        &["maio"],
        &["junho"],
        &["julho"],
        &["agosto"],
        &["setembro"],
        &["outubro"],
        &["novembro"],
        &["dezembro"],
    ],
    weekdays: [
        &["segunda"],
        &["terça", "terca"],
        &["quarta"],
        &["quinta"],
        &["sexta"],
        &["sábado", "sabado"],
        &["domingo"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["hoje"],
    tomorrow: &["amanhã", "amanha"],
    yesterday: &["ontem"],
    in_word: &["em", "daqui"],
    at_word: &["às", "as"],
    am: &["am"],
    pm: &["pm"],
    units: &[
        ("m", DurationUnit::Minute),
        ("min", DurationUnit::Minute),
        ("minuto", DurationUnit::Minute),
        ("minutos", DurationUnit::Minute),
        ("h", DurationUnit::Hour),
        ("hora", DurationUnit::Hour),
        ("horas", DurationUnit::Hour),
        ("d", DurationUnit::Day),
        ("dia", DurationUnit::Day),
        ("dias", DurationUnit::Day),
        ("semana", DurationUnit::Week),
        ("semanas", DurationUnit::Week),
        ("mês", DurationUnit::Month),
        ("mes", DurationUnit::Month),
        ("meses", DurationUnit::Month),
    ],
    morning: &["manhã", "manha"],
    afternoon: &["tarde"],
    evening: &["noite"],
    tonight: &["esta noite"],
    next_word: &[],
    this_word: &[],
    next_week: &["próxima semana", "proxima semana"],
    end_of_day: &["fim do dia"],
    at_due: &["no vencimento"],
    before_due_suffixes: &["antes do vencimento"],
    after_word: &["depois"],
    compact: false,
};

pub const GERMAN: Vocabulary = Vocabulary {
    tag: "de",
    months: [
        &["januar"],
        &["februar"],
        &["märz", "maerz"],
        &["april"],
        &["mai"],
        &["juni"],
        &["juli"],
        &["august"],
        &["september"],
        &["oktober"],
        &["november"],
        &["dezember"],
    ],
    weekdays: [
        &["montag"],
        &["dienstag"],
        &["mittwoch"],
        &["donnerstag"],
        &["freitag"],
        &["samstag"],
        &["sonntag"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["heute"],
    tomorrow: &["morgen"],
    yesterday: &["gestern"],
    in_word: &["in"],
    at_word: &["um"],
    am: &["am"],
    pm: &["pm"],
    units: &[
        ("m", DurationUnit::Minute),
        ("min", DurationUnit::Minute),
        ("minute", DurationUnit::Minute),
        ("minuten", DurationUnit::Minute),
        ("h", DurationUnit::Hour),
        ("stunde", DurationUnit::Hour),
        ("stunden", DurationUnit::Hour),
        ("t", DurationUnit::Day),
        ("tag", DurationUnit::Day),
        ("tage", DurationUnit::Day),
        ("w", DurationUnit::Week),
        ("woche", DurationUnit::Week),
        ("wochen", DurationUnit::Week),
        ("monat", DurationUnit::Month),
        ("monate", DurationUnit::Month),
    ],
    morning: &["morgens"],
    afternoon: &["nachmittags"],
    evening: &["abends"],
    tonight: &["heute abend"],
    next_word: &[],
    this_word: &[],
    next_week: &["nächste woche", "naechste woche"],
    end_of_day: &["tagesende", "ende des tages"],
    at_due: &["bei fälligkeit", "bei faelligkeit"],
    before_due_suffixes: &["vor fälligkeit", "vor faelligkeit"],
    after_word: &["später", "spaeter"],
    compact: false,
};

pub const FRENCH: Vocabulary = Vocabulary {
    tag: "fr",
    months: [
        &["janvier"],
        &["février", "fevrier"],
        &["mars"],
        &["avril"],
        &["mai"],
        &["juin"],
        &["juillet"],
        &["août", "aout"],
        &["septembre"],
        &["octobre"],
        &["novembre"],
        &["décembre", "decembre"],
    ],
    weekdays: [
        &["lundi"],
        &["mardi"],
        &["mercredi"],
        &["jeudi"],
        &["vendredi"],
        &["samedi"],
        &["dimanche"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["aujourd'hui"],
    tomorrow: &["demain"],
    yesterday: &["hier"],
    in_word: &["dans"],
    at_word: &["à", "a"],
    am: &["am"],
    pm: &["pm"],
    units: &[
        ("m", DurationUnit::Minute),
        ("min", DurationUnit::Minute),
        ("minute", DurationUnit::Minute),
        ("minutes", DurationUnit::Minute),
        ("h", DurationUnit::Hour),
        ("heure", DurationUnit::Hour),
        ("heures", DurationUnit::Hour),
        ("j", DurationUnit::Day),
        ("jour", DurationUnit::Day),
        ("jours", DurationUnit::Day),
        ("semaine", DurationUnit::Week),
        ("semaines", DurationUnit::Week),
        ("mois", DurationUnit::Month),
    ],
    morning: &["matin"],
    afternoon: &["après-midi", "apres-midi"],
    evening: &["soir"],
    tonight: &["ce soir"],
    next_word: &[],
    this_word: &[],
    next_week: &["la semaine prochaine", "semaine prochaine"],
    end_of_day: &["fin de journée", "fin de journee"],
    at_due: &["à l'échéance", "a l'echeance"],
    before_due_suffixes: &["avant l'échéance", "avant l'echeance"],
    after_word: &["plus tard"],
    compact: false,
};

pub const ARABIC: Vocabulary = Vocabulary {
    tag: "ar",
    months: [
        &["يناير"],
        &["فبراير"],
        &["مارس"],
        &["أبريل", "ابريل"],
        &["مايو"],
        &["يونيو"],
        &["يوليو"],
        &["أغسطس", "اغسطس"],
        &["سبتمبر"],
        &["أكتوبر", "اكتوبر"],
        &["نوفمبر"],
        &["ديسمبر"],
    ],
    weekdays: [
        &["الاثنين"],
        &["الثلاثاء"],
        &["الأربعاء", "الاربعاء"],
        &["الخميس"],
        &["الجمعة"],
        &["السبت"],
        &["الأحد", "الاحد"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["اليوم"],
    tomorrow: &["غدًا", "غدا"],
    yesterday: &["أمس", "امس"],
    in_word: &["خلال", "بعد"],
    at_word: &["الساعة"],
    am: &["ص"],
    pm: &["م"],
    units: &[
        ("دقيقة", DurationUnit::Minute),
        ("دقائق", DurationUnit::Minute),
        ("ساعة", DurationUnit::Hour),
        ("ساعات", DurationUnit::Hour),
        ("يوم", DurationUnit::Day),
        ("أيام", DurationUnit::Day),
        ("ايام", DurationUnit::Day),
        ("أسبوع", DurationUnit::Week),
        ("اسبوع", DurationUnit::Week),
        ("أسابيع", DurationUnit::Week),
        ("اسابيع", DurationUnit::Week),
        ("شهر", DurationUnit::Month),
        ("أشهر", DurationUnit::Month),
        ("اشهر", DurationUnit::Month),
    ],
    morning: &["صباحًا", "صباحا"],
    afternoon: &["بعد الظهر"],
    evening: &["مساءً", "مساء"],
    tonight: &["الليلة"],
    next_word: &[],
    this_word: &[],
    next_week: &["الأسبوع القادم", "الاسبوع القادم"],
    end_of_day: &["نهاية اليوم"],
    at_due: &["عند الاستحقاق"],
    before_due_suffixes: &["قبل الاستحقاق"],
    after_word: &["لاحقًا", "لاحقا"],
    compact: false,
};

pub const CHINESE_SIMPLIFIED: Vocabulary = Vocabulary {
    tag: "zh-Hans",
    months: [
        &["一月"],
        &["二月"],
        &["三月"],
        &["四月"],
        &["五月"],
        &["六月"],
        &["七月"],
        &["八月"],
        &["九月"],
        &["十月"],
        &["十一月"],
        &["十二月"],
    ],
    weekdays: [
        &["星期一"],
        &["星期二"],
        &["星期三"],
        &["星期四"],
        &["星期五"],
        &["星期六"],
        &["星期日"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["今天"],
    tomorrow: &["明天"],
    yesterday: &["昨天"],
    in_word: &["在"],
    at_word: &["在"],
    am: &["上午"],
    pm: &["下午"],
    units: &[
        ("分钟", DurationUnit::Minute),
        ("小时", DurationUnit::Hour),
        ("天", DurationUnit::Day),
        ("周", DurationUnit::Week),
        ("星期", DurationUnit::Week),
        ("个月", DurationUnit::Month),
    ],
    morning: &["早上", "上午"],
    afternoon: &["下午"],
    evening: &["晚上"],
    tonight: &["今晚"],
    next_word: &[],
    this_word: &[],
    next_week: &["下周", "下星期"],
    end_of_day: &["今天结束", "一天结束时"],
    at_due: &["到期时"],
    before_due_suffixes: &["到期前"],
    after_word: &["后"],
    compact: true,
};

pub const JAPANESE: Vocabulary = Vocabulary {
    tag: "ja",
    months: [
        &["1月"],
        &["2月"],
        &["3月"],
        &["4月"],
        &["5月"],
        &["6月"],
        &["7月"],
        &["8月"],
        &["9月"],
        &["10月"],
        &["11月"],
        &["12月"],
    ],
    weekdays: [
        &["月曜日"],
        &["火曜日"],
        &["水曜日"],
        &["木曜日"],
        &["金曜日"],
        &["土曜日"],
        &["日曜日"],
    ],
    min_abbrev: 1,
    prefix_names: false,
    today: &["今日"],
    tomorrow: &["明日"],
    yesterday: &["昨日"],
    in_word: &["後"],
    at_word: &["に"],
    am: &["午前"],
    pm: &["午後"],
    units: &[
        ("分", DurationUnit::Minute),
        ("時間", DurationUnit::Hour),
        ("日", DurationUnit::Day),
        ("週間", DurationUnit::Week),
        ("週", DurationUnit::Week),
        ("か月", DurationUnit::Month),
        ("ヶ月", DurationUnit::Month),
    ],
    morning: &["朝", "午前"],
    afternoon: &["午後"],
    evening: &["夕方"],
    tonight: &["今夜"],
    next_word: &[],
    this_word: &[],
    next_week: &["来週"],
    end_of_day: &["今日の終わり"],
    at_due: &["期限時"],
    before_due_suffixes: &["期限前"],
    after_word: &["後"],
    compact: true,
};

/// The eight language tables Koru supports from the beginning.
pub const ALL: [&Vocabulary; 8] = [
    &ENGLISH,
    &SPANISH,
    &PORTUGUESE_BRAZIL,
    &GERMAN,
    &FRENCH,
    &ARABIC,
    &CHINESE_SIMPLIFIED,
    &JAPANESE,
];

impl Vocabulary {
    /// The 1-based month `token` abbreviates, if any.
    pub fn month(&self, token: &str) -> Option<u32> {
        self.by_prefix(&self.months, token).map(|i| i as u32 + 1)
    }

    /// The weekday `token` abbreviates, as days from Monday.
    pub fn weekday(&self, token: &str) -> Option<usize> {
        self.by_prefix(&self.weekdays, token)
    }

    /// A duration unit — whole-token, never abbreviated further.
    pub fn unit(&self, token: &str) -> Option<DurationUnit> {
        self.units
            .iter()
            .find(|(name, _)| *name == token)
            .map(|(_, unit)| *unit)
    }

    /// Whether `token` is one of a keyword list, matched whole.
    pub fn is(list: &[&str], token: &str) -> bool {
        list.contains(&token)
    }

    /// A lone `am`/`pm`, which the tokenizer may glue onto the number in front
    /// of it.
    pub fn is_meridiem(&self, token: &str) -> bool {
        Self::is(self.am, token) || Self::is(self.pm, token)
    }

    /// Split a meridiem suffix off a clock token: `("9", Some(false))` for
    /// `9am`, `("9:30", Some(true))` for `9:30pm`, `("17:30", None)` for a
    /// 24-hour reading. The bool is *is afternoon*.
    pub fn split_meridiem<'a>(&self, token: &'a str) -> (&'a str, Option<bool>) {
        for name in self.pm {
            if let Some(body) = token.strip_suffix(name) {
                return (body, Some(true));
            }
        }
        for name in self.am {
            if let Some(body) = token.strip_suffix(name) {
                return (body, Some(false));
            }
        }
        (token, None)
    }

    /// The index of the first group holding a spelling `token` is a prefix of.
    ///
    /// Shorter than [`Vocabulary::min_abbrev`] characters answers `None`,
    /// which is the whole of the ambiguity policy — see the module note.
    fn by_prefix(&self, groups: &[&'static [&'static str]], token: &str) -> Option<usize> {
        if token.chars().count() < self.min_abbrev {
            return None;
        }
        groups.iter().position(|names| {
            names.iter().any(|name| {
                if self.prefix_names {
                    name.starts_with(token)
                } else {
                    *name == token
                }
            })
        })
    }
}
