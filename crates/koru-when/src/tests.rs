// SPDX-License-Identifier: MPL-2.0

//! The suite is the spec. Every accepted form in the crate-level grammar has a
//! row here, and so does every refusal.
//!
//! **Both reference points are always injected and always different**, which is
//! the one structural rule of this file. `elsewhere()` puts the selection in
//! November while the clock stays in August, so a rule that reaches for the
//! wrong one cannot pass by coincidence — which is exactly how the `+` change
//! would otherwise slip through, since the two agree whenever today is what is
//! selected.

use super::*;

/// The injected clock: **Wednesday 26 August 2026, 10:15**. A midweek morning,
/// so weekday and meridiem arithmetic both have room on either side.
fn now() -> NaiveDateTime {
    at(2026, 8, 26, 10, 15)
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
    day(y, m, d).and_hms_opt(h, min, 0).unwrap()
}

/// Nothing selected — the selection sits on the clock, which is the state in
/// which a sign changes nothing.
fn here() -> Reference {
    Reference::without_selection(now())
}

/// **A selection well away from today**: Friday 13 November 2026, read at the
/// current time of day. Every signed phrase is asserted against this, so using
/// `now` where `selection` was meant is a failing test rather than a passing
/// coincidence.
fn elsewhere() -> Reference {
    Reference::on_day(now(), day(2026, 11, 13))
}

fn p(input: &str) -> Option<When> {
    parse(input, here(), DateOrder::default())
}

/// The pre-Piece-1 public contract in one frozen table. The focused tests
/// below explain each rule; this table makes an API refactor prove that every
/// accepted value, refusal, reference point, and slash-order result survived
/// together through both public entry points.
#[test]
fn legacy_parse_and_parse_in_regression_goldens() {
    let reference = elsewhere();
    let cases = [
        ("today", Some(When::Date(day(2026, 8, 26)))),
        ("tomorrow", Some(When::Date(day(2026, 8, 27)))),
        ("friday", Some(When::Date(day(2026, 8, 28)))),
        ("aug 31", Some(When::Date(day(2026, 8, 31)))),
        ("31 august", Some(When::Date(day(2026, 8, 31)))),
        ("aug 31 2027", Some(When::Date(day(2027, 8, 31)))),
        ("tomorrow 9am", Some(When::Wall(at(2026, 8, 27, 9, 0)))),
        ("friday at 5 pm", Some(When::Wall(at(2026, 8, 28, 17, 0)))),
        ("5pm", Some(When::Wall(at(2026, 8, 26, 17, 0)))),
        ("9am", Some(When::Wall(at(2026, 8, 27, 9, 0)))),
        ("30m", Some(When::Wall(at(2026, 8, 26, 10, 45)))),
        ("in 4h", Some(When::Wall(at(2026, 8, 26, 14, 15)))),
        ("2w", Some(When::Date(day(2026, 9, 9)))),
        ("+3d", Some(When::Date(day(2026, 11, 16)))),
        ("-90m", Some(When::Wall(at(2026, 11, 13, 8, 45)))),
        ("10/28", Some(When::Date(day(2026, 10, 28)))),
        ("10/11", Some(When::Date(day(2026, 10, 11)))),
        ("", None),
        ("9", None),
        ("in +2w", None),
        // Taught on 2026-11-05: the weekday in next week (Monday-started).
        ("next friday", Some(When::Date(day(2026, 9, 4)))),
        ("2027-10-28", None),
        ("tomorrow 9am extra", None),
        ("EST", None),
    ];

    for (input, expected) in cases {
        assert_eq!(
            parse(input, reference, DateOrder::MonthFirst),
            expected,
            "parse({input:?})"
        );
        assert_eq!(
            parse_in(input, reference, DateOrder::MonthFirst, &ENGLISH),
            expected,
            "parse_in({input:?})"
        );
    }

    assert_eq!(
        parse("10/11", reference, DateOrder::DayFirst),
        Some(When::Date(day(2026, 11, 10))),
        "the caller-supplied slash order is part of the compatibility contract"
    );
}

/// A phrase that names a **day**. Panics if the grammar answered a wall clock,
/// which is the distinction these tests exist to keep.
fn date(input: &str) -> NaiveDate {
    date_from(input, here())
}

/// A phrase that names a **wall clock** — floating, with no zone. This crate
/// has no way to answer an instant, and that is the point (ADR-183, rule 4).
fn wall(input: &str) -> NaiveDateTime {
    wall_from(input, here())
}

fn date_from(input: &str, reference: Reference) -> NaiveDate {
    match parse(input, reference, DateOrder::default()) {
        Some(When::Date(d)) => d,
        other => panic!("{input:?} parsed to {other:?}, expected a date"),
    }
}

fn wall_from(input: &str, reference: Reference) -> NaiveDateTime {
    match parse(input, reference, DateOrder::default()) {
        Some(When::Wall(t)) => t,
        other => panic!("{input:?} parsed to {other:?}, expected a wall clock"),
    }
}

/// A slash date under an explicit order.
fn ordered(input: &str, order: DateOrder) -> NaiveDate {
    match parse(input, here(), order) {
        Some(When::Date(d)) => d,
        other => panic!("{input:?} parsed to {other:?}, expected a date"),
    }
}

// ---------------------------------------------------------------- day words

#[test]
fn the_day_words_are_relative_to_the_injected_clock() {
    assert_eq!(date("today"), day(2026, 8, 26));
    assert_eq!(date("tomorrow"), day(2026, 8, 27));
    assert_eq!(date("yesterday"), day(2026, 8, 25));
    assert_eq!(
        date("  Tomorrow  "),
        day(2026, 8, 27),
        "case and whitespace"
    );
}

/// A day word reads the **clock**, never the selection — only a signed
/// duration is relative to what is highlighted.
#[test]
fn a_day_word_ignores_the_selection() {
    assert_eq!(date_from("today", elsewhere()), day(2026, 8, 26));
    assert_eq!(date_from("tomorrow", elsewhere()), day(2026, 8, 27));
    assert_eq!(date_from("friday", elsewhere()), day(2026, 8, 28));
    assert_eq!(date_from("aug 31", elsewhere()), day(2026, 8, 31));
}

/// A weekday is always the **next** occurrence — strictly after today, so
/// naming today's own weekday means a week from now. 26 Aug 2026 is a
/// Wednesday.
#[test]
fn a_weekday_names_the_next_occurrence_and_its_own_day_means_next_week() {
    assert_eq!(date("friday"), day(2026, 8, 28));
    assert_eq!(date("fri"), day(2026, 8, 28));
    assert_eq!(date("monday"), day(2026, 8, 31));
    assert_eq!(date("tuesday"), day(2026, 9, 1), "crosses the month");
    assert_eq!(date("tues"), day(2026, 9, 1));
    assert_eq!(date("thurs"), day(2026, 8, 27));
    assert_eq!(date("thur"), day(2026, 8, 27));
    assert_eq!(date("sat"), day(2026, 8, 29));
    assert_eq!(date("sun"), day(2026, 8, 30));
    assert_eq!(
        date("wednesday"),
        day(2026, 9, 2),
        "today's weekday is next week's, never today"
    );
}

// ------------------------------------------------------------- month + day

#[test]
fn month_day_reads_both_ways_round_and_rolls_a_past_date_to_next_year() {
    assert_eq!(date("aug 31"), day(2026, 8, 31));
    assert_eq!(date("august 31"), day(2026, 8, 31));
    assert_eq!(date("31 aug"), day(2026, 8, 31));
    assert_eq!(date("31 august"), day(2026, 8, 31));
    // Today, named by month and day, is today — only a *past* date rolls.
    assert_eq!(date("aug 26"), day(2026, 8, 26));
    assert_eq!(date("jan 5"), day(2027, 1, 5), "already past, so next year");
    assert_eq!(
        date("aug 25"),
        day(2027, 8, 25),
        "yesterday's date rolls too"
    );
}

/// **Abbreviation is a rule, not a list.** Any prefix of three characters or
/// more parses, which is where `sept 10` comes from — the editor's parser
/// carried a literal rewrite of that one string, and there is now nothing to
/// forget for the next four-letter form.
#[test]
fn a_month_abbreviates_to_any_prefix_of_three_characters_or_more() {
    assert_eq!(date("sept 10"), day(2026, 9, 10), "the four-letter form");
    assert_eq!(date("sep 10"), day(2026, 9, 10));
    assert_eq!(date("septem 10"), day(2026, 9, 10));
    assert_eq!(date("september 10"), day(2026, 9, 10));
    assert_eq!(date("10 sept"), day(2026, 9, 10), "and the other way round");
    assert_eq!(date("janu 5"), day(2027, 1, 5));
    assert_eq!(date("febr 3"), day(2027, 2, 3));
    assert_eq!(
        date("may 30"),
        day(2027, 5, 30),
        "a name that is its own abbreviation"
    );
    assert_eq!(
        p("ju 5"),
        None,
        "two letters cannot choose between June and July"
    );
    assert_eq!(p("s 10"), None);
}

/// The prefix rule is only safe while no two names share one, so this asserts
/// it rather than assuming it — the check a second language has to pass.
#[test]
fn every_month_prefix_resolves_to_its_own_month() {
    for (index, spellings) in ENGLISH.months.iter().enumerate() {
        for name in *spellings {
            for len in ENGLISH.min_abbrev..=name.chars().count() {
                let prefix: String = name.chars().take(len).collect();
                assert_eq!(
                    ENGLISH.month(&prefix),
                    Some(index as u32 + 1),
                    "{prefix:?} should be month {}",
                    index + 1
                );
            }
        }
    }
    assert_eq!(ENGLISH.month("ju"), None, "below the abbreviation floor");
    assert_eq!(ENGLISH.month("smarch"), None);
}

#[test]
fn every_weekday_prefix_resolves_to_its_own_weekday() {
    for (index, spellings) in ENGLISH.weekdays.iter().enumerate() {
        for name in *spellings {
            for len in ENGLISH.min_abbrev..=name.chars().count() {
                let prefix: String = name.chars().take(len).collect();
                assert_eq!(ENGLISH.weekday(&prefix), Some(index), "{prefix:?}");
            }
        }
    }
    assert_eq!(ENGLISH.weekday("mo"), None, "below the abbreviation floor");
}

/// Weekdays are tried before months, so a token that abbreviated both would
/// silently lose one of them. English has none; a language that does needs a
/// longer `min_abbrev` or a disambiguated spelling.
#[test]
fn no_token_abbreviates_both_a_month_and_a_weekday() {
    for spellings in ENGLISH.months.iter().chain(ENGLISH.weekdays.iter()) {
        for name in *spellings {
            for len in ENGLISH.min_abbrev..=name.chars().count() {
                let prefix: String = name.chars().take(len).collect();
                assert!(
                    ENGLISH.month(&prefix).is_none() || ENGLISH.weekday(&prefix).is_none(),
                    "{prefix:?} is both"
                );
            }
        }
    }
}

#[test]
fn an_explicit_year_is_taken_as_written() {
    assert_eq!(date("aug 31 2027"), day(2027, 8, 31));
    assert_eq!(date("aug 31, 2027"), day(2027, 8, 31), "comma tolerated");
    assert_eq!(date("jan 5 2025"), day(2025, 1, 5), "even a past one");
    assert_eq!(date("feb 29 2028"), day(2028, 2, 29), "a real leap day");
    assert_eq!(p("feb 29 2027"), None, "a fake one is refused");
}

/// Feb 29 with no year: 2027 has none, so the roll lands nowhere and the
/// phrase is refused rather than silently moved to March.
#[test]
fn a_rolled_date_that_does_not_exist_is_refused() {
    let late = Reference::without_selection(at(2026, 12, 1, 9, 0));
    assert_eq!(parse("feb 29", late, DateOrder::default()), None);
    assert_eq!(
        parse("2/29", late, DateOrder::default()),
        None,
        "and by slash"
    );
}

// ------------------------------------------------------------------- times

#[test]
fn a_time_makes_a_wall_clock() {
    assert_eq!(wall("tomorrow 9am"), at(2026, 8, 27, 9, 0));
    assert_eq!(wall("friday 17:30"), at(2026, 8, 28, 17, 30));
    assert_eq!(wall("aug 31 at 9:15pm"), at(2026, 8, 31, 21, 15));
    assert_eq!(
        wall("today 12am"),
        at(2026, 8, 26, 0, 0),
        "12am is midnight"
    );
    assert_eq!(wall("today 12pm"), at(2026, 8, 26, 12, 0), "12pm is noon");
}

#[test]
fn malformed_times_refuse_rather_than_guess() {
    assert_eq!(p("tomorrow 25:00"), None, "no 25th hour");
    assert_eq!(p("tomorrow 13pm"), None, "13pm is not a time");
    assert_eq!(p("tomorrow 9:75"), None, "no 75th minute");
    assert_eq!(p("tomorrow 9"), None, "bare digits are ambiguous");
    assert_eq!(p("tomorrow 9am extra"), None, "trailing words");
}

/// **A time on its own is today at that hour**, and every accepted spelling is
/// the one the attached form already took: `am`/`pm`, or `HH:MM`.
#[test]
fn a_bare_time_is_today_at_that_hour() {
    assert_eq!(wall("5pm"), at(2026, 8, 26, 17, 0));
    assert_eq!(wall("17:30"), at(2026, 8, 26, 17, 30));
    assert_eq!(wall("9:15pm"), at(2026, 8, 26, 21, 15));
    assert_eq!(
        wall("  5PM "),
        at(2026, 8, 26, 17, 0),
        "case and whitespace"
    );
    assert_eq!(wall("11pm"), at(2026, 8, 26, 23, 0));
}

/// …and tomorrow at it once it has gone by. The clock is 10:15, so the two
/// sides of the boundary are a minute apart, and the boundary itself — the
/// current minute exactly — counts as past.
#[test]
fn a_bare_time_that_has_passed_rolls_to_tomorrow() {
    assert_eq!(
        wall("10:16"),
        at(2026, 8, 26, 10, 16),
        "one minute ahead is still today"
    );
    assert_eq!(
        wall("10:15"),
        at(2026, 8, 27, 10, 15),
        "now itself is not in the future"
    );
    assert_eq!(
        wall("10:14"),
        at(2026, 8, 27, 10, 14),
        "one minute behind is tomorrow"
    );
    assert_eq!(wall("9am"), at(2026, 8, 27, 9, 0), "this morning has gone");
    assert_eq!(
        wall("12am"),
        at(2026, 8, 27, 0, 0),
        "midnight is always behind a working clock"
    );
    assert_eq!(
        wall("12pm"),
        at(2026, 8, 26, 12, 0),
        "noon is still ahead at 10:15"
    );
    // The roll follows the clock, never the selection.
    assert_eq!(wall_from("9am", elsewhere()), at(2026, 8, 27, 9, 0));
}

/// **`4h` is a duration and `4pm` is a time**, and the two readings must not
/// take each other's input.
#[test]
fn a_bare_duration_and_a_bare_time_do_not_collide() {
    assert_eq!(wall("4h"), at(2026, 8, 26, 14, 15), "four hours from 10:15");
    assert_eq!(
        wall("4pm"),
        at(2026, 8, 26, 16, 0),
        "the hour named, not four hours on"
    );
    assert_eq!(wall("30m"), at(2026, 8, 26, 10, 45));
    // `2mo` is months and `2:00` is a clock; neither may read the other.
    assert_eq!(date("2mo"), day(2026, 10, 26));
    assert_eq!(
        wall("2:00"),
        at(2026, 8, 27, 2, 0),
        "02:00 has gone by, so tomorrow"
    );
}

/// The ambiguity guard: a standalone token is a time only with a meridiem or a
/// colon, so nothing that could be a day-of-month, a year or a bare hour
/// parses.
#[test]
fn a_bare_number_is_never_a_time() {
    for phrase in [
        "2", "5", "9", "17", "31", "2026", "+31", "+9", "-9", "0:", ":30", "25:00", "13pm", "9:75",
        "12:60", "24:00",
    ] {
        assert_eq!(p(phrase), None, "{phrase:?} should not parse");
    }
}

// ------------------------------------------------------- the spaced meridiem

/// **A space before `am`/`pm` changes nothing.** The join happens once, in
/// `tokenize`, so every form that took the glued spelling takes the spaced one.
#[test]
fn a_meridiem_written_with_a_space_is_the_same_time() {
    for (spaced, glued) in [
        ("5 pm", "5pm"),
        ("9 am", "9am"),
        ("12 pm", "12pm"),
        ("12 am", "12am"),
        ("9:15 pm", "9:15pm"),
        ("  5 PM ", "5pm"),
    ] {
        assert_eq!(wall(spaced), wall(glued), "{spaced:?}");
    }
    // The exact values, so this does not only assert two spellings agree —
    // including both sides of the today/tomorrow boundary at 10:15.
    assert_eq!(wall("5 pm"), at(2026, 8, 26, 17, 0), "this afternoon");
    assert_eq!(wall("9 am"), at(2026, 8, 27, 9, 0), "this morning has gone");
    assert_eq!(wall("12 pm"), at(2026, 8, 26, 12, 0), "noon is still ahead");
    assert_eq!(
        wall("12 am"),
        at(2026, 8, 27, 0, 0),
        "midnight is always behind"
    );
}

#[test]
fn a_spaced_meridiem_attaches_to_a_day_as_well() {
    assert_eq!(wall("friday 5 pm"), at(2026, 8, 28, 17, 0));
    assert_eq!(wall("friday at 5 pm"), at(2026, 8, 28, 17, 0));
    assert_eq!(wall("aug 31 at 9 pm"), at(2026, 8, 31, 21, 0));
    assert_eq!(wall("tomorrow 9 am"), at(2026, 8, 27, 9, 0));
    assert_eq!(
        wall("31 aug at 9:15 pm"),
        at(2026, 8, 31, 21, 15),
        "the other way round, and past a date the join must not have touched"
    );
}

/// **The join only ever adds a meridiem**, so nothing that was refused before
/// it can be accepted after.
#[test]
fn joining_a_meridiem_does_not_weaken_the_guard() {
    for phrase in [
        "pm",
        "am",
        "pm 5",
        "am 9",
        "5 pm pm",
        "+5 pm",
        "13 pm",
        "25 pm",
        "31 am",
        "0 am",
        "aug pm",
        "5 pm extra",
        "friday 5 pm pm",
        "10/28 pm",
    ] {
        assert_eq!(p(phrase), None, "{phrase:?} should not parse");
    }
    assert_eq!(p("5"), None);
    assert_eq!(p("31"), None);
    assert_eq!(
        date("31 aug"),
        day(2026, 8, 31),
        "still a date, not a clock"
    );
}

// --------------------------------------------------------------- durations

/// Sub-day durations are wall clocks; day-and-longer are calendar dates —
/// "in 3d" names a day, not 10:15 three days from now.
#[test]
fn durations_split_at_the_day_boundary() {
    assert_eq!(wall("in 30m"), at(2026, 8, 26, 10, 45));
    assert_eq!(wall("in 1h"), at(2026, 8, 26, 11, 15));
    assert_eq!(
        wall("in 14h"),
        at(2026, 8, 27, 0, 15),
        "a wall clock crosses midnight; a date never carries the clock"
    );
    assert_eq!(date("in 3d"), day(2026, 8, 29));
    assert_eq!(date("in 2w"), day(2026, 9, 9), "crosses the month");
    assert_eq!(date("in 2mo"), day(2026, 10, 26));
    assert_eq!(date("in 2 weeks"), day(2026, 9, 9), "spelled-out unit");
    // Every unit spelling, in both shapes.
    for (short, long) in [
        ("30m", "30 minutes"),
        ("30min", "30 mins"),
        ("4h", "4 hours"),
        ("4hr", "4 hrs"),
    ] {
        assert_eq!(wall(short), wall(long), "{short:?} vs {long:?}");
    }
    for (short, long) in [
        ("3d", "3 days"),
        ("2w", "2 weeks"),
        ("2wk", "2 wks"),
        ("2mo", "2 months"),
        ("2mos", "2 month"),
    ] {
        assert_eq!(date(short), date(long), "{short:?} vs {long:?}");
    }
}

/// **A spaced duration is the `<n> <unit>` arm**, and the meridiem join
/// neither created it nor may take it away.
#[test]
fn a_spaced_duration_is_unchanged() {
    assert_eq!(wall("4 h"), wall("4h"));
    assert_eq!(wall("in 4 h"), wall("in 4h"));
    assert_eq!(wall("4 h"), at(2026, 8, 26, 14, 15));
    assert_eq!(date("2 weeks"), day(2026, 9, 9));
    assert_eq!(date("3 days"), day(2026, 8, 29));
}

/// An unsigned duration counts from **now**, with or without the preposition.
#[test]
fn an_unsigned_duration_counts_from_the_clock() {
    for (bare, with_in) in [("4h", "in 4h"), ("30m", "in 30m")] {
        assert_eq!(
            wall_from(bare, elsewhere()),
            wall_from(with_in, elsewhere())
        );
    }
    // …and from the clock even when something else is selected.
    assert_eq!(wall_from("4h", elsewhere()), at(2026, 8, 26, 14, 15));
    assert_eq!(date_from("2w", elsewhere()), day(2026, 9, 9));
    assert_eq!(date_from("in 2w", elsewhere()), day(2026, 9, 9));
    assert_eq!(date_from("in 2mo", elsewhere()), day(2026, 10, 26));
}

/// **`+` binds to the selection, and that is a reversal.** It used to be
/// decoration — `+4h` and `4h` were the same phrase. Under the two-reference
/// rule `+2w` is two weeks from the day the caller is looking at, which is the
/// only reading under which a phrase box and the calendar beside it agree.
#[test]
fn a_plus_counts_forward_from_the_selection() {
    let sel = elsewhere();
    assert_eq!(date_from("+2w", sel), day(2026, 11, 27));
    assert_eq!(date_from("+3d", sel), day(2026, 11, 16));
    assert_eq!(date_from("+1mo", sel), day(2026, 12, 13));
    assert_eq!(date_from("+2 weeks", sel), day(2026, 11, 27));
    assert_eq!(wall_from("+4h", sel), at(2026, 11, 13, 14, 15));
    assert_eq!(wall_from("+30m", sel), at(2026, 11, 13, 10, 45));
    // The reversal, stated as a difference rather than a value: the same
    // phrase without the sign lands three months away.
    assert_ne!(wall_from("+4h", sel), wall_from("4h", sel));
    assert_eq!(wall_from("4h", sel), at(2026, 8, 26, 14, 15));
}

#[test]
fn a_minus_counts_backward_from_the_selection() {
    let sel = elsewhere();
    assert_eq!(date_from("-2w", sel), day(2026, 10, 30));
    assert_eq!(date_from("-1d", sel), day(2026, 11, 12));
    assert_eq!(date_from("-1mo", sel), day(2026, 10, 13));
    assert_eq!(date_from("-2 weeks", sel), day(2026, 10, 30));
    assert_eq!(wall_from("-4h", sel), at(2026, 11, 13, 6, 15));
    assert_eq!(wall_from("-90m", sel), at(2026, 11, 13, 8, 45));
}

/// A signed duration keeps the same day-or-clock split as an unsigned one —
/// the sign chooses the reference point and nothing else.
#[test]
fn a_signed_duration_keeps_the_day_and_clock_split() {
    let sel = elsewhere();
    assert!(matches!(
        parse("+3d", sel, DateOrder::default()),
        Some(When::Date(_))
    ));
    assert!(matches!(
        parse("-3d", sel, DateOrder::default()),
        Some(When::Date(_))
    ));
    assert!(matches!(
        parse("+4h", sel, DateOrder::default()),
        Some(When::Wall(_))
    ));
    assert!(matches!(
        parse("-90m", sel, DateOrder::default()),
        Some(When::Wall(_))
    ));
}

/// A sub-day shift moves along the selection's own clock, so it crosses
/// midnight into the next day rather than wrapping inside the selected one.
#[test]
fn a_signed_sub_day_shift_crosses_midnight() {
    let late = Reference::new(now(), at(2026, 11, 13, 23, 50));
    assert_eq!(wall_from("+30m", late), at(2026, 11, 14, 0, 20));
    let early = Reference::new(now(), at(2026, 11, 13, 0, 10));
    assert_eq!(wall_from("-30m", early), at(2026, 11, 12, 23, 40));
}

/// With today selected the two reference points coincide, so a sign changes
/// nothing — which is what makes the reversal invisible until a selection
/// moves, and why every test above sets one.
#[test]
fn with_today_selected_a_sign_changes_nothing() {
    assert_eq!(wall("+4h"), wall("4h"));
    assert_eq!(wall("+4h"), wall("in 4h"));
    assert_eq!(date("+2w"), date("2w"));
    assert_eq!(date("+2 weeks"), date("in 2 weeks"));
    assert_eq!(wall("+4h"), at(2026, 8, 26, 14, 15));
    assert_eq!(date("-2w"), day(2026, 8, 12), "and back the other way");
    // `on_day` with today is the same state, reached the way a picker reaches
    // it: the selected *day*, read at the current time.
    let today_selected = Reference::on_day(now(), day(2026, 8, 26));
    assert_eq!(wall_from("+4h", today_selected), wall("4h"));
}

/// `in` names the clock and a sign names the selection, so the two contradict
/// each other. Refused rather than one silently winning.
#[test]
fn in_and_a_sign_contradict_each_other() {
    assert_eq!(p("in +2w"), None);
    assert_eq!(p("in -2w"), None);
    assert_eq!(p("in +4h"), None);
    assert_eq!(p("in -2 weeks"), None);
}

/// A sign opens a duration and nothing else.
#[test]
fn a_sign_opens_a_duration_and_nothing_else() {
    for phrase in [
        "+",
        "-",
        "+31",
        "+9",
        "+2026",
        "+friday",
        "-tomorrow",
        "+aug 31",
        "+10/28",
        "+5 pm",
        "-today",
    ] {
        assert_eq!(p(phrase), None, "{phrase:?} should not parse");
    }
}

/// The bare form must not swallow a month-day phrase that happens to start
/// with a number. `31 aug` reaches the duration reader first and is handed
/// straight back, because `aug` is not a unit.
#[test]
fn a_leading_number_is_still_a_month_day_when_the_unit_is_a_month_name() {
    assert_eq!(date("31 aug"), day(2026, 8, 31));
    assert_eq!(date("31 august"), day(2026, 8, 31));
    assert_eq!(date("1 sep"), day(2026, 9, 1));
    assert_eq!(
        date("2 may"),
        day(2027, 5, 2),
        "a month name that is also short"
    );
    assert_eq!(p("31"), None);
    assert_eq!(p("+31"), None);
}

/// Month arithmetic clamps the day, in both directions and through a leap
/// year — the three ways month shifting is usually wrong.
#[test]
fn a_month_duration_clamps_short_months() {
    let jan31 = Reference::without_selection(at(2026, 1, 31, 9, 0));
    assert_eq!(date_from("in 1mo", jan31), day(2026, 2, 28));
    assert_eq!(date_from("1mo", jan31), day(2026, 2, 28));

    let leap = Reference::new(now(), at(2028, 1, 31, 9, 0));
    assert_eq!(
        date_from("+1mo", leap),
        day(2028, 2, 29),
        "a leap February has the 29th to clamp to"
    );

    let mar31 = Reference::new(now(), at(2026, 3, 31, 9, 0));
    assert_eq!(
        date_from("-1mo", mar31),
        day(2026, 2, 28),
        "and backwards over the same short month"
    );

    let dec = Reference::new(now(), at(2026, 12, 5, 9, 0));
    assert_eq!(date_from("+1mo", dec), day(2027, 1, 5), "crosses the year");
    let jan = Reference::new(now(), at(2026, 1, 5, 9, 0));
    assert_eq!(date_from("-1mo", jan), day(2025, 12, 5), "and back over it");
}

#[test]
fn an_absurd_duration_is_refused() {
    assert_eq!(p("in 10001d"), None);
    assert_eq!(p("+99999w"), None);
    assert_eq!(p("in 0d"), None);
    assert_eq!(p("0m"), None);
}

// ------------------------------------------------------------ slash dates

/// **An unambiguous pair reads the same under either setting.** A preference
/// that turned `28/10` into the 28th month would be a preference for being
/// wrong, so the setting is only ever the tie-breaker.
#[test]
fn an_unambiguous_slash_pair_ignores_the_setting() {
    for order in [
        DateOrder::MonthFirst,
        DateOrder::DayFirst,
        DateOrder::YearFirst,
    ] {
        assert_eq!(ordered("10/28", order), day(2026, 10, 28), "{order:?}");
        assert_eq!(ordered("28/10", order), day(2026, 10, 28), "{order:?}");
        assert_eq!(ordered("13/10", order), day(2026, 10, 13), "{order:?}");
        assert_eq!(ordered("10/31", order), day(2026, 10, 31), "{order:?}");
        assert_eq!(ordered("31/12", order), day(2026, 12, 31), "{order:?}");
    }
}

/// Only a pair where **both** numbers could be months follows the setting.
#[test]
fn an_ambiguous_slash_pair_follows_the_setting() {
    assert_eq!(ordered("10/11", DateOrder::MonthFirst), day(2026, 10, 11));
    assert_eq!(ordered("10/11", DateOrder::DayFirst), day(2026, 11, 10));
    assert_eq!(ordered("12/1", DateOrder::MonthFirst), day(2026, 12, 1));
    assert_eq!(ordered("12/1", DateOrder::DayFirst), day(2027, 1, 12));
    // A yearless ambiguous pair rolls under both readings, independently.
    assert_eq!(ordered("1/2", DateOrder::MonthFirst), day(2027, 1, 2));
    assert_eq!(ordered("1/2", DateOrder::DayFirst), day(2027, 2, 1));
    assert_eq!(
        parse("10/11", here(), DateOrder::YearFirst),
        None,
        "year-first order cannot guess the missing year or month position"
    );
}

#[test]
fn year_first_requires_and_accepts_a_four_digit_leading_year() {
    for input in ["2027-10-28", "2027/10/28"] {
        assert_eq!(
            ordered(input, DateOrder::YearFirst),
            day(2027, 10, 28),
            "{input}"
        );
        assert_eq!(
            parse_expression(input, Language::English, DateOrder::YearFirst),
            Some(TemporalExpression::Date(DateExpression::MonthDay {
                month: 10,
                day: 28,
                year: Some(2027),
            })),
            "expression parser: {input}"
        );
    }
    for input in [
        "27-10-28",
        "2027-28-10",
        "2027-10",
        "2027-10/28",
        "10/28/2027",
    ] {
        assert_eq!(parse(input, here(), DateOrder::YearFirst), None, "{input}");
        assert_eq!(
            parse_expression(input, Language::English, DateOrder::YearFirst),
            None,
            "expression parser: {input}"
        );
    }
    assert_eq!(
        parse("2027-10-28", here(), DateOrder::MonthFirst),
        None,
        "other locale orders do not acquire ISO semantics"
    );
}

#[test]
fn the_default_order_is_month_first_and_lives_in_one_constant() {
    assert_eq!(DEFAULT_DATE_ORDER, DateOrder::MonthFirst);
    assert_eq!(DateOrder::default(), DEFAULT_DATE_ORDER);
    assert_eq!(
        date("10/11"),
        day(2026, 10, 11),
        "the default, through parse"
    );
}

/// A year is accepted **in third position only**, and only as four digits.
#[test]
fn a_slash_date_takes_a_four_digit_year_in_third_position() {
    assert_eq!(
        ordered("10/28/2027", DateOrder::MonthFirst),
        day(2027, 10, 28)
    );
    assert_eq!(
        ordered("28/10/2027", DateOrder::MonthFirst),
        day(2027, 10, 28)
    );
    assert_eq!(
        ordered("10/11/2027", DateOrder::MonthFirst),
        day(2027, 10, 11)
    );
    assert_eq!(
        ordered("10/11/2027", DateOrder::DayFirst),
        day(2027, 11, 10)
    );
    assert_eq!(
        date("1/5/2025"),
        day(2025, 1, 5),
        "a past year is taken as written"
    );
    assert_eq!(date("2/29/2028"), day(2028, 2, 29), "a real leap day");
    assert_eq!(p("2/29/2027"), None, "a fake one is refused");
}

/// A yearless slash date rolls forward exactly as `jan 5` does — one rule, one
/// implementation.
#[test]
fn a_yearless_slash_date_rolls_like_a_month_day() {
    assert_eq!(date("1/5"), date("jan 5"));
    assert_eq!(date("1/5"), day(2027, 1, 5));
    assert_eq!(date("8/26"), day(2026, 8, 26), "today does not roll");
    assert_eq!(date("8/25"), day(2027, 8, 25), "yesterday does");
}

#[test]
fn a_slash_date_takes_a_time_like_any_other_day() {
    assert_eq!(wall("10/28 5pm"), at(2026, 10, 28, 17, 0));
    assert_eq!(wall("10/28 at 5 pm"), at(2026, 10, 28, 17, 0));
    assert_eq!(wall("10/28/2027 9am"), at(2027, 10, 28, 9, 0));
    assert_eq!(wall("28/10 17:30"), at(2026, 10, 28, 17, 30));
}

#[test]
fn malformed_slash_dates_refuse() {
    for phrase in [
        "2027/10/28", // a leading year is not a position this grammar reads
        "10/28/27",   // two digits is also a day; the century is not ours to guess
        "10/28/2027/1",
        "13/13",
        "0/5",
        "5/0",
        "10/32",
        "32/10",
        "10/",
        "/10",
        "//",
        "10/ab",
        "10/28/abcd",
    ] {
        assert_eq!(p(phrase), None, "{phrase:?} should not parse");
    }
}

// ------------------------------------------------------------------ the rest

#[test]
fn what_the_grammar_does_not_know_is_none() {
    for phrase in [
        "",
        "   ",
        "someday",
        "next", // a modifier with no weekday is nothing
        "in",
        "in h",
        "in 2fortnights",
        "aug",
        "aug 32",
        "31 31",
        "friday tomorrow",
        "2027-10-28", // ISO is not grammar either
        "sept.",
        "end of month",
    ] {
        assert_eq!(p(phrase), None, "{phrase:?} should not parse");
    }
}

/// The result type has two arms and neither can carry a zone — the property
/// the committing layer depends on ([docs/architecture/TIME.md](../../../docs/architecture/TIME.md)).
#[test]
fn a_phrase_answers_a_day_or_a_wall_clock_and_never_an_instant() {
    assert!(matches!(p("friday"), Some(When::Date(_))));
    assert!(matches!(p("friday 5pm"), Some(When::Wall(_))));
    assert!(matches!(p("in 3d"), Some(When::Date(_))));
    assert!(matches!(p("in 3h"), Some(When::Wall(_))));
    assert!(matches!(p("10/28"), Some(When::Date(_))));
    assert!(matches!(p("10/28 9am"), Some(When::Wall(_))));
}

/// `parse` is `parse_in` against the one table there is. The seam exists so a
/// second language is a `const`, not a rewrite.
#[test]
fn the_english_table_is_the_one_parse_uses() {
    assert_eq!(ENGLISH.tag, "en");
    assert_eq!(
        parse_in("sept 10", here(), DateOrder::default(), &ENGLISH),
        parse("sept 10", here(), DateOrder::default())
    );
}

/// **`next` and `this` modify any weekday** — one rule over the table's
/// weekdays, not a phrase list. On Thursday 5 November 2026: *friday* and
/// *this friday* are the 6th, the coming one; *next friday* is the 13th, the
/// Friday of next week; and a weekday named on its own day is still a week
/// on, with or without `this`.
#[test]
fn next_and_this_modify_any_weekday() {
    let thursday =
        Reference::without_selection(day(2026, 11, 5).and_hms_opt(10, 0, 0).expect("a time"));
    let on = |phrase: &str| parse(phrase, thursday, DateOrder::default());
    assert_eq!(on("friday"), Some(When::Date(day(2026, 11, 6))));
    assert_eq!(on("this friday"), Some(When::Date(day(2026, 11, 6))));
    assert_eq!(on("coming fri"), Some(When::Date(day(2026, 11, 6))));
    assert_eq!(on("next friday"), Some(When::Date(day(2026, 11, 13))));
    assert_eq!(on("next fri"), Some(When::Date(day(2026, 11, 13))));
    assert_eq!(on("next monday"), Some(When::Date(day(2026, 11, 9))));
    assert_eq!(on("next thursday"), Some(When::Date(day(2026, 11, 12))));
    assert_eq!(on("this thursday"), Some(When::Date(day(2026, 11, 12))));
    assert_eq!(
        on("next friday 9am"),
        Some(When::Wall(day(2026, 11, 13).and_hms_opt(9, 0, 0).unwrap())),
        "with a time, like any day phrase"
    );
    assert_eq!(on("next"), None);
    assert_eq!(
        parse_expression("next friday", Language::English, DateOrder::default()).and_then(|e| {
            match e {
                TemporalExpression::Date(date) => Some(date),
                _ => None,
            }
        }),
        Some(DateExpression::NextWeekday(4)),
        "unresolved, the week's start is left to the resolver"
    );
}
