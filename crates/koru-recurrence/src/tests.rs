// SPDX-License-Identifier: MPL-2.0

use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};
use koru_time_zone::IanaZoneRules;

use super::*;

fn epoch(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_millis()
}

fn utc() -> IanaZoneRules {
    IanaZoneRules::parse("UTC").unwrap()
}

#[test]
fn routine_detail_fixed_next_respects_now_effective_dst() {
    let cron = CronExpression::parse("30 2 * * *").unwrap();
    let zone = CanonicalZone::parse("America/New_York").unwrap();
    let now = epoch("2026-03-08T07:00:00Z");
    let next = next_zoned_after_at_least(&cron, &zone, now, now)
        .unwrap()
        .unwrap();
    assert_eq!(next.instant_ms(), epoch("2026-03-08T07:30:00Z"));
    assert_eq!(
        next_zoned_after_at_least(&cron, &zone, now, next.instant_ms()).unwrap(),
        Some(next)
    );
    assert!(
        next_zoned_after_at_least(&cron, &zone, next.instant_ms(), 0)
            .unwrap()
            .unwrap()
            .instant_ms()
            > next.instant_ms()
    );
    let lower = epoch("2026-03-10T07:00:00Z");
    assert!(
        next_zoned_after_at_least(&cron, &zone, now, lower)
            .unwrap()
            .unwrap()
            .instant_ms()
            >= lower
    );
    let cron = CronExpression::parse("30 1 * * *").unwrap();
    let fold = next_zoned_after_at_least(&cron, &zone, epoch("2026-11-01T04:00:00Z"), 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        fold.instant_ms(),
        zoned("30 1 * * *", "America/New_York")
            .next_after(epoch("2026-11-01T04:00:00Z"), &utc())
            .unwrap()
            .unwrap()
            .instant_ms()
    );
}
#[test]
fn routine_detail_fixed_next_refuses_extreme_horizon() {
    let cron = CronExpression::parse("*/1 * * * *").unwrap();
    let zone = CanonicalZone::parse("UTC").unwrap();
    for (now, lower) in [(i64::MIN, i64::MIN), (i64::MAX, 0), (0, i64::MAX)] {
        assert!(next_zoned_after_at_least(&cron, &zone, now, lower).is_err());
    }
    let next = next_zoned_after_at_least(&cron, &zone, -60_001, -60_000)
        .unwrap()
        .unwrap();
    // The owning cron explorer starts its year domain at1970. Preserve that
    // horizon rather than inventing a pre-epoch occurrence in this wrapper.
    assert_eq!(next.instant_ms(), 0);
    assert_eq!(
        next,
        zoned("*/1 * * * *", "UTC")
            .next_after(-60_001, &utc())
            .unwrap()
            .unwrap()
    );
    assert!(next.instant_ms() > -60_001 && next.instant_ms() >= -60_000);
    assert_eq!(
        next_zoned_after_at_least(&cron, &zone, -60_000, -60_000)
            .unwrap()
            .unwrap()
            .instant_ms(),
        0
    );
}

fn floating(expression: &str) -> RecurrenceRule {
    RecurrenceRule::floating(CronExpression::parse(expression).unwrap())
}

fn zoned(expression: &str, zone: &str) -> RecurrenceRule {
    RecurrenceRule::zoned(CronExpression::parse(expression).unwrap(), zone).unwrap()
}

fn collect_year(rule: &RecurrenceRule, zone: &IanaZoneRules) -> Vec<Occurrence> {
    let start = epoch("2027-01-01T00:00:00Z");
    let end = epoch("2028-01-01T00:00:00Z");
    let mut cursor = start - 1;
    let mut occurrences = Vec::new();
    loop {
        let occurrence = rule.next_after(cursor, zone).unwrap().unwrap();
        if occurrence.instant_ms() >= end {
            break;
        }
        assert!(occurrence.instant_ms() >= start);
        assert!(occurrence.instant_ms() > cursor);
        assert_eq!(occurrence.instant_ms() % OCCURRENCE_PRECISION_MS, 0);
        occurrences.push(occurrence);
        cursor = occurrence.instant_ms();
    }
    occurrences
}

#[test]
fn every_supported_shape_enumerates_a_full_year() {
    let zone = utc();

    let hourly = floating("17 * * * *");
    let occurrences = collect_year(&hourly, &zone);
    assert_eq!(hourly.cron().shape(), RuleShape::Hourly);
    assert_eq!(occurrences.len(), 8_760);
    assert!(occurrences.iter().all(|occurrence| {
        DateTime::<Utc>::from_timestamp_millis(occurrence.instant_ms())
            .unwrap()
            .minute()
            == 17
    }));

    let daily = floating("5 9 * * *");
    let occurrences = collect_year(&daily, &zone);
    assert_eq!(daily.cron().shape(), RuleShape::Daily);
    assert_eq!(occurrences.len(), 365);
    assert!(occurrences.iter().all(|occurrence| {
        let value = DateTime::<Utc>::from_timestamp_millis(occurrence.instant_ms()).unwrap();
        (value.hour(), value.minute()) == (9, 5)
    }));

    let weekly = floating("0 9 * * MON-FRI");
    let occurrences = collect_year(&weekly, &zone);
    assert_eq!(weekly.cron().shape(), RuleShape::Weekly);
    assert_eq!(occurrences.len(), 261);
    assert!(occurrences.iter().all(|occurrence| {
        let weekday = DateTime::<Utc>::from_timestamp_millis(occurrence.instant_ms())
            .unwrap()
            .weekday()
            .number_from_monday();
        weekday <= 5
    }));

    let monthly = floating("30 14 15 * *");
    let occurrences = collect_year(&monthly, &zone);
    assert_eq!(monthly.cron().shape(), RuleShape::Monthly);
    assert_eq!(occurrences.len(), 12);
    assert!(occurrences.iter().all(|occurrence| {
        DateTime::<Utc>::from_timestamp_millis(occurrence.instant_ms())
            .unwrap()
            .day()
            == 15
    }));
}

#[test]
fn stored_cron_has_one_canonical_spelling_and_a_bounded_language() {
    let cron = CronExpression::parse("05   09 * * mon,fri").unwrap();
    assert_eq!(cron.as_str(), "5 9 * * Mon,Fri");
    assert_eq!(cron.shape(), RuleShape::Weekly);
    assert_eq!(
        CronExpression::parse("0 9 * * FRI,MON,WED")
            .unwrap()
            .as_str(),
        "0 9 * * Mon,Wed,Fri"
    );
    assert_eq!(
        CronExpression::parse("0 9 * * MON-FRI").unwrap().as_str(),
        "0 9 * * Mon,Tue,Wed,Thu,Fri"
    );
    let every_five = CronExpression::parse("*/05 * * * *").unwrap();
    assert_eq!(every_five.as_str(), "*/5 * * * *");
    assert_eq!(every_five.shape(), RuleShape::Hourly);
    assert_eq!(every_five.parts().minute_pattern(), MinutePattern::Every(5));
    let every_minute = CronExpression::parse("* * * * *").unwrap();
    assert_eq!(every_minute.as_str(), "*/1 * * * *");
    assert_eq!(
        every_minute.parts().minute_pattern(),
        MinutePattern::Every(1)
    );

    let named_minutes = CronExpression::parse("45,05,30 * * * *").unwrap();
    assert_eq!(named_minutes.as_str(), "5,30,45 * * * *");
    assert!(matches!(
        named_minutes.parts().minute_pattern(),
        MinutePattern::List(minutes)
            if minutes.iter().collect::<Vec<_>>() == vec![5, 30, 45]
    ));

    for invalid in [
        "0 0 9 * * *",
        "*/0 * * * *",
        "*/60 * * * *",
        "*/5 9 * * *",
        "5,,30 * * * *",
        "5,5 * * * *",
        "5,60 * * * *",
        "5,30 9 * * *",
        "0 9 * JAN *",
        "0 9 * * 1",
        "0 9 1 * MON",
        "0,30 9 * * *",
        "@daily",
        "0 * 1 * *",
        "0 9 * * FRI-MON",
    ] {
        assert!(
            CronExpression::parse(invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

fn assert_picker_round_trip(parts: CronParts) {
    let from_picker = CronExpression::try_from(parts).unwrap();
    assert_eq!(from_picker.parts(), parts);
    assert_eq!(from_picker.shape(), parts.shape());

    let from_advanced = CronExpression::parse(from_picker.as_str()).unwrap();
    assert_eq!(from_advanced.parts(), parts);
    assert_eq!(from_advanced, from_picker);
}

#[test]
fn every_picker_state_round_trips_through_the_canonical_cron() {
    for minute in 0..=59 {
        assert_picker_round_trip(CronParts::hourly(minute).unwrap());
    }

    for hour in 0..=23 {
        for minute in 0..=59 {
            assert_picker_round_trip(CronParts::daily(hour, minute).unwrap());
        }
    }

    for day in 1..=31 {
        for hour in 0..=23 {
            for minute in 0..=59 {
                assert_picker_round_trip(CronParts::monthly(day, hour, minute).unwrap());
            }
        }
    }

    for bits in 1..=0b0111_1111 {
        let weekdays = WeekdaySet::from_bits(bits).unwrap();
        for hour in 0..=23 {
            for minute in 0..=59 {
                assert_picker_round_trip(CronParts::weekly(weekdays, hour, minute).unwrap());
            }
        }
    }
}

#[test]
fn advanced_spellings_rebuild_the_same_picker_state_and_canonical_rule() {
    let weekdays = WeekdaySet::from_days([
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
    ])
    .unwrap();
    let cases = [
        ("05   * * * *", CronParts::hourly(5).unwrap(), "5 * * * *"),
        ("05  09 * * *", CronParts::daily(9, 5).unwrap(), "5 9 * * *"),
        (
            "00 09 * * mon-fri",
            CronParts::weekly(weekdays, 9, 0).unwrap(),
            "0 9 * * Mon,Tue,Wed,Thu,Fri",
        ),
        (
            "030 014 015 * *",
            CronParts::monthly(15, 14, 30).unwrap(),
            "30 14 15 * *",
        ),
    ];

    for (input, parts, canonical) in cases {
        let parsed = CronExpression::parse(input).unwrap();
        assert_eq!(parsed.parts(), parts, "{input}");
        assert_eq!(parsed.as_str(), canonical, "{input}");
        assert_eq!(CronExpression::try_from(parts).unwrap(), parsed, "{input}");
    }
}

#[test]
fn picker_constructors_enforce_the_stored_dialect_bounds() {
    assert!(matches!(
        CronParts::hourly(60),
        Err(DialectError::OutOfRange {
            field: "minute",
            minimum: 0,
            maximum: 59,
        })
    ));
    assert!(matches!(
        CronParts::daily(24, 0),
        Err(DialectError::OutOfRange {
            field: "hour",
            minimum: 0,
            maximum: 23,
        })
    ));
    assert_eq!(
        CronParts::every_minutes(0),
        Err(DialectError::OutOfRange {
            field: "minute step",
            minimum: 1,
            maximum: 59,
        })
    );
    assert_eq!(
        CronParts::every_minutes(60),
        Err(DialectError::OutOfRange {
            field: "minute step",
            minimum: 1,
            maximum: 59,
        })
    );
    assert_eq!(
        CronParts::hourly_list([5]),
        Err(DialectError::InvalidMinutePattern)
    );
    assert_eq!(
        CronParts::hourly_list([5, 5]),
        Err(DialectError::InvalidMinutePattern)
    );
    assert!(matches!(
        CronParts::hourly_list([5, 60]),
        Err(DialectError::OutOfRange {
            field: "minute",
            minimum: 0,
            maximum: 59,
        })
    ));
    assert_picker_round_trip(CronParts::every_minutes(1).unwrap());
    let named_minutes = CronParts::hourly_list([45, 5, 30]).unwrap();
    assert_eq!(
        CronExpression::try_from(named_minutes).unwrap().as_str(),
        "5,30,45 * * * *"
    );
    assert_picker_round_trip(named_minutes);
    for day in [0, 32] {
        assert!(matches!(
            CronParts::monthly(day, 0, 0),
            Err(DialectError::OutOfRange {
                field: "day-of-month",
                minimum: 1,
                maximum: 31,
            })
        ));
    }
    assert_eq!(
        WeekdaySet::from_days(std::iter::empty()),
        Err(DialectError::InvalidWeekday)
    );
    assert_eq!(WeekdaySet::from_bits(0), Err(DialectError::InvalidWeekday));
    assert_eq!(
        WeekdaySet::from_bits(0b1000_0000),
        Err(DialectError::InvalidWeekday)
    );
}

#[test]
fn weekday_sets_are_semantic_and_iterate_in_canonical_order() {
    let selected = WeekdaySet::from_days([
        Weekday::Sunday,
        Weekday::Wednesday,
        Weekday::Monday,
        Weekday::Wednesday,
    ])
    .unwrap();
    assert_eq!(selected.bits(), 0b100_0101);
    assert!(selected.contains(Weekday::Sunday));
    assert!(!selected.contains(Weekday::Friday));
    assert_eq!(
        selected.iter().collect::<Vec<_>>(),
        vec![Weekday::Monday, Weekday::Wednesday, Weekday::Sunday]
    );
    assert_eq!(
        CronExpression::try_from(CronParts::weekly(selected, 9, 0).unwrap())
            .unwrap()
            .as_str(),
        "0 9 * * Mon,Wed,Sun"
    );
}

#[test]
fn serde_cannot_bypass_cron_or_zone_validation() {
    let rule = zoned("0 9 * * MON-FRI", "US/Eastern");
    let json = serde_json::to_string(&rule).unwrap();
    assert_eq!(
        json,
        r#"{"type":"zoned","cron":"0 9 * * Mon,Tue,Wed,Thu,Fri","zone":"America/New_York"}"#
    );
    assert_eq!(serde_json::from_str::<RecurrenceRule>(&json).unwrap(), rule);

    let interval =
        serde_json::from_str::<RecurrenceRule>(r#"{"type":"floating","cron":"*/5 * * * *"}"#)
            .unwrap();
    assert_eq!(
        serde_json::to_string(&interval).unwrap(),
        r#"{"type":"floating","cron":"*/5 * * * *"}"#
    );
    assert!(
        serde_json::from_str::<RecurrenceRule>(
            r#"{"type":"zoned","cron":"0 9 * * *","zone":"EST"}"#
        )
        .is_err()
    );
}

#[test]
fn minute_intervals_and_lists_advance_at_their_exact_minutes() {
    let zone = utc();
    let every_five = floating("*/5 * * * *");
    assert_eq!(
        every_five
            .next_after(epoch("2027-01-01T00:00:00Z"), &zone)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2027-01-01T00:05:00Z")
    );
    assert_eq!(
        every_five
            .next_after(epoch("2027-01-01T00:58:00Z"), &zone)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2027-01-01T01:00:00Z")
    );

    let named_minutes = floating("5,30,45 * * * *");
    for (after, expected) in [
        ("2027-01-01T00:05:00Z", "2027-01-01T00:30:00Z"),
        ("2027-01-01T00:30:00Z", "2027-01-01T00:45:00Z"),
        ("2027-01-01T00:45:00Z", "2027-01-01T01:05:00Z"),
    ] {
        assert_eq!(
            named_minutes
                .next_after(epoch(after), &zone)
                .unwrap()
                .unwrap()
                .instant_ms(),
            epoch(expected)
        );
    }
}

#[test]
fn new_york_gap_and_fold_are_exactly_the_timezone_mechanisms_outcomes() {
    let rules = IanaZoneRules::parse("America/New_York").unwrap();

    let gap = zoned("30 2 * * *", "America/New_York")
        .next_after(epoch("2026-03-07T08:00:00Z"), &utc())
        .unwrap()
        .unwrap();
    let expected_gap = rules.resolve_canonical_wall("2026-03-08T02:30:00").unwrap();
    assert_eq!(gap.instant_ms(), expected_gap.instant_ms);
    assert_eq!(gap.instant_ms(), epoch("2026-03-08T07:30:00Z"));

    let fold = zoned("30 1 * * *", "America/New_York")
        .next_after(epoch("2026-10-31T06:00:00Z"), &utc())
        .unwrap()
        .unwrap();
    let expected_fold = rules.resolve_canonical_wall("2026-11-01T01:30:00").unwrap();
    assert_eq!(fold.instant_ms(), expected_fold.instant_ms);
    assert_eq!(fold.instant_ms(), epoch("2026-11-01T05:30:00Z"));
}

#[test]
fn southern_hemisphere_gap_and_fold_follow_the_same_shared_policy() {
    let rules = IanaZoneRules::parse("Australia/Sydney").unwrap();

    let fold = zoned("30 2 * * *", "Australia/Sydney")
        .next_after(epoch("2026-04-03T18:00:00Z"), &utc())
        .unwrap()
        .unwrap();
    let expected_fold = rules.resolve_canonical_wall("2026-04-05T02:30:00").unwrap();
    assert_eq!(fold.instant_ms(), expected_fold.instant_ms);
    assert_eq!(fold.instant_ms(), epoch("2026-04-04T15:30:00Z"));

    let gap = zoned("30 2 * * *", "Australia/Sydney")
        .next_after(epoch("2026-10-02T18:00:00Z"), &utc())
        .unwrap()
        .unwrap();
    let expected_gap = rules.resolve_canonical_wall("2026-10-04T02:30:00").unwrap();
    assert_eq!(gap.instant_ms(), expected_gap.instant_ms);
    assert_eq!(gap.instant_ms(), epoch("2026-10-03T16:30:00Z"));
}

#[test]
fn a_gap_candidate_still_fires_when_its_shifted_instant_is_ahead() {
    let new_york = IanaZoneRules::parse("America/New_York").unwrap();
    let next = zoned("30 2 * * *", "America/New_York")
        .next_after(epoch("2026-03-08T07:00:00Z"), &new_york)
        .unwrap()
        .unwrap();
    assert_eq!(next.instant_ms(), epoch("2026-03-08T07:30:00Z"));
}

#[test]
fn floating_follows_the_evaluation_zone_while_zoned_keeps_its_zone() {
    let tokyo = IanaZoneRules::parse("Asia/Tokyo").unwrap();
    let new_york = IanaZoneRules::parse("America/New_York").unwrap();
    let reference = epoch("2026-07-01T00:00:00Z");

    let floating = floating("0 9 * * *");
    assert_eq!(
        floating
            .next_after(reference, &tokyo)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2026-07-02T00:00:00Z")
    );
    assert_eq!(
        floating
            .next_after(reference, &new_york)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2026-07-01T13:00:00Z")
    );

    let fixed = zoned("0 9 * * *", "America/New_York");
    assert_eq!(
        fixed.next_after(reference, &tokyo).unwrap(),
        fixed.next_after(reference, &new_york).unwrap()
    );
}

#[test]
fn lateness_boundary_and_clock_rollback_are_explicit_and_stateless() {
    let occurrence = Occurrence::from_instant_ms(epoch("2026-07-01T09:00:00Z")).unwrap();
    assert_eq!(
        occurrence.timing_at(epoch("2026-07-01T08:59:59.999Z")),
        OccurrenceTiming::Upcoming
    );
    assert_eq!(
        occurrence.timing_at(epoch("2026-07-01T09:00:59.999Z")),
        OccurrenceTiming::Due
    );
    assert_eq!(
        occurrence.timing_at(epoch("2026-07-01T09:01:00Z")),
        OccurrenceTiming::Missed
    );
    assert_eq!(
        occurrence.timing_at(epoch("2026-07-01T08:00:00Z")),
        OccurrenceTiming::Upcoming,
        "a rolled-back injected clock cannot mutate evaluator state"
    );

    let rule = floating("0 9 * * *");
    let zone = utc();
    let reference = epoch("2026-07-01T08:00:00Z");
    assert_eq!(
        rule.next_after(reference, &zone).unwrap(),
        rule.next_after(reference, &zone).unwrap(),
        "evaluation is a pure function of explicit inputs"
    );
    assert_eq!(
        rule.next_after(epoch("2026-07-01T10:00:00Z"), &zone)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2026-07-02T09:00:00Z")
    );
    assert_eq!(
        rule.next_after(reference, &zone)
            .unwrap()
            .unwrap()
            .instant_ms(),
        epoch("2026-07-01T09:00:00Z"),
        "moving the supplied reference backward recomputes, not remembers"
    );
}

#[test]
fn references_outside_chronos_calendar_are_refused() {
    assert_eq!(
        floating("0 9 * * *").next_after(i64::MAX, &utc()),
        Err(EvaluationError::InvalidInstant)
    );
    assert_eq!(
        Occurrence::from_instant_ms(epoch("2026-07-01T09:00:00.001Z")),
        Err(EvaluationError::NotMinuteAligned)
    );
}

#[test]
fn generated_occurrences_are_always_canonical_whole_minutes() {
    let rule = floating("7 * * * *");
    let occurrence = rule
        .next_after(epoch("2026-07-01T08:07:00.001Z"), &utc())
        .unwrap()
        .unwrap();
    assert_eq!(occurrence.instant_ms(), epoch("2026-07-01T09:07:00Z"));
    let wall = NaiveDateTime::parse_from_str("2026-07-01T09:07:00", "%Y-%m-%dT%H:%M:%S").unwrap();
    assert_eq!(occurrence.instant_ms(), wall.and_utc().timestamp_millis());
}
