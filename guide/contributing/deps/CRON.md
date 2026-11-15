# `cron` in Koru

`cron` 0.17 parses expressions and explores their candidate dates for
`koru-recurrence`. It does not define Koru's stored language, timezone meaning,
or firing policy. No `cron` type crosses the crate's public API.

## The field mismatch is load-bearing

The dependency reads seven fields:

```text
second minute hour day-of-month month day-of-week year
```

Koru stores a bounded five-field expression:

```text
minute hour day-of-month month day-of-week
```

`koru-recurrence` validates and canonicalizes its own expression first, then
adds a zero-second field and wildcard year internally. Passing user or stored
text directly to `cron::Schedule::from_str` would silently expand the product
language to ranges, unrestricted lists and steps, seconds, years, month
filters, special weekday forms, and combinations Koru has not given semantics.

The picker supports four shapes, and the advanced field adds two bounded
hourly minute selectors:

| Shape | Stored form | Example |
| --- | --- | --- |
| hourly | `M * * * *` | `17 * * * *` |
| every N minutes | `*/N * * * *` | `*/5 * * * *` |
| named minutes | `M1,M2,... * * * *` | `5,30,45 * * * *` |
| daily | `M H * * *` | `0 9 * * *` |
| weekly | `M H * * WEEKDAYS` | `0 9 * * Mon,Tue,Wed,Thu,Fri` |
| monthly | `M H D * *` | `30 14 15 * *` |

Weekdays are three-letter names, as one inclusive range or a comma-separated
set. Both inputs canonicalize to a Monday-to-Sunday comma list, so equivalent
accepted rules have one stored spelling. Numeric weekdays are deliberately
refused because cron implementations disagree about whether zero or seven
means Sunday.

`N` is 1 through 59. A minute list contains two or more distinct values from 0
through 59 and canonicalizes into ascending order. Steps and lists are valid
only when hour, day, month and weekday are all `*`; Koru does not admit the
dependency's general selector grammar through this narrow addition. ADR-403
records the boundary.

## Picker state crosses a semantic seam

`CronExpression::parts()` exposes validated `CronParts`, with a typed
`MinutePattern` and a non-empty `WeekdaySet` made from the semantic `Weekday`
enum. Daily, weekly and monthly constructors always carry one literal minute;
checked hourly constructors also admit an interval or minute set. The reverse
direction is `CronExpression::from_parts` (or `TryFrom<CronParts>`), which
produces the one canonical five-field spelling and performs the same dependency
check as parsed text.

A structured picker must use that seam. It must not split `as_str()` itself,
and it must not manufacture cron text and feed it back through the advanced
field. Conversely, accepted advanced text updates the semantic parts; a step or
minute list leaves all four simple picker chips unselected because none
represents it. Choosing a chip explicitly returns to the simple picker domain.
Invalid text leaves the last valid semantic value alone. This keeps a range
such as `MON-FRI`, an unordered selector and the picker from becoming competing
authorities for the same stored rule.

The typed parts contain no timezone. Floating versus zoned is a property of
`RecurrenceRule`, orthogonal to the five cron fields, and moving it into picker
state would collapse that temporal distinction.

## A dependency date is a wall candidate, not an instant

The explorer is run with a UTC `DateTime`, but its fields are treated as an
otherwise-zone-free wall reading. `koru-recurrence` then resolves that reading
through `koru-time-zone::IanaZoneRules`. Do not parameterize the explorer with
the actual IANA zone: that would give the dependency ownership of gap and fold
semantics and create a second temporal policy beside Koru's zone rules.

Candidate exploration begins two wall-clock hours before the supplied
reference and filters resolved instants strictly after it. That lookback is
required on a spring gap: at 03:00, the wall candidate 02:30 has resolved to
03:30 and is still in the future. The densest supported rule is every minute;
the two-hour lookback crosses about 120 candidates, and a 512-candidate cap
turns any non-advancing or unexpectedly dense dependency behavior into a named
error.

The crate never calls `Utc::now`, `Local::now`, `SystemTime`, or an OS timezone
detector. Both the reference epoch millisecond and the floating rule's IANA
rules are arguments.

## The explorer has a finite year domain

The locked `cron` 0.17.0 `Years` owner ranges from 1970 through 2100 inclusive.
Koru's five-field dialect supplies `*` for that year field; it does not widen
the range. A signed pre-epoch observation is a valid input, but the next
every-minute UTC occurrence after `-60001` is `0`, not `-60000`. Preserve this
owner horizon in wrappers and tests. An unavailable/no-next forecast is not
permission to borrow a viewer zone or invent an occurrence beyond the explorer.
