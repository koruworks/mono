// SPDX-License-Identifier: MPL-2.0

use std::fmt;

use chrono::{DateTime, TimeDelta, Utc};
use koru_core::time::ZoneRules;
use koru_time_zone::{IanaZoneRules, ZoneError};
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::CronExpression;

/// Recurrence occurrences are exact to one minute.
pub const OCCURRENCE_PRECISION_MS: i64 = 60_000;
const DST_LOOKBACK_HOURS: i64 = 2;
const MAX_LOOKBACK_CANDIDATES: usize = 512;

/// A persisted recurrence rule. The variants preserve Koru's existing
/// floating-versus-zoned distinction without an optional timezone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecurrenceRule {
    Floating {
        cron: CronExpression,
    },
    Zoned {
        cron: CronExpression,
        zone: CanonicalZone,
    },
}

impl RecurrenceRule {
    pub const fn floating(cron: CronExpression) -> Self {
        Self::Floating { cron }
    }

    /// Make a zoned rule, validating against the compiled IANA database and
    /// retaining its canonical name rather than an alias.
    pub fn zoned(cron: CronExpression, zone: &str) -> Result<Self, EvaluationError> {
        Ok(Self::Zoned {
            cron,
            zone: CanonicalZone::parse(zone)?,
        })
    }

    pub const fn cron(&self) -> &CronExpression {
        match self {
            Self::Floating { cron } | Self::Zoned { cron, .. } => cron,
        }
    }

    pub fn zone_name(&self) -> Option<&str> {
        match self {
            Self::Floating { .. } => None,
            Self::Zoned { zone, .. } => Some(zone.as_str()),
        }
    }

    /// Find the first occurrence strictly after `reference_ms`.
    ///
    /// A floating rule is interpreted in `floating_zone`, normally the
    /// device's current IANA zone. A zoned rule ignores that presentation/input
    /// context and resolves in its retained semantic zone. No clock is read.
    pub fn next_after<R: ZoneRules + ?Sized>(
        &self,
        reference_ms: i64,
        floating_zone: &R,
    ) -> Result<Option<Occurrence>, EvaluationError> {
        match self {
            Self::Floating { .. } => next_after(self.cron(), reference_ms, floating_zone),
            Self::Zoned { zone, .. } => {
                let rules = IanaZoneRules::parse(zone.as_str()).map_err(EvaluationError::Zone)?;
                next_after(self.cron(), reference_ms, &rules)
            }
        }
    }
}

/// Canonical, database-validated IANA name. Deserialization passes through the
/// same validation as authored rules, so persistence cannot bypass the edge.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CanonicalZone(String);

#[cfg(feature = "schema")]
impl schemars::JsonSchema for CanonicalZone {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CanonicalZone".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A canonical IANA time-zone name."
        })
    }
}

impl CanonicalZone {
    /// Validate and canonicalize through the existing compiled IANA owner.
    pub fn parse(value: &str) -> Result<Self, EvaluationError> {
        let rules = IanaZoneRules::parse(value).map_err(EvaluationError::Zone)?;
        Ok(Self(rules.canonical_name().as_str().to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Forecast one fixed-zone occurrence strictly after the observation and not
/// before the definition lower bound. No clock, claim or floating viewer zone.
pub fn next_zoned_after_at_least(
    cron: &CronExpression,
    zone: &CanonicalZone,
    observed_at_ms: i64,
    effective_at_ms: i64,
) -> Result<Option<Occurrence>, EvaluationError> {
    let reference = if effective_at_ms > observed_at_ms {
        effective_at_ms
            .checked_sub(1)
            .ok_or(EvaluationError::InvalidInstant)?
    } else {
        observed_at_ms
    };
    let rules = IanaZoneRules::parse(zone.as_str()).map_err(EvaluationError::Zone)?;
    next_after(cron, reference, &rules)
}

impl<'de> Deserialize<'de> for CanonicalZone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Occurrence {
    instant_ms: i64,
}

impl Occurrence {
    /// Reconstitute a persisted occurrence identity without weakening minute
    /// precision.
    pub fn from_instant_ms(instant_ms: i64) -> Result<Self, EvaluationError> {
        DateTime::<Utc>::from_timestamp_millis(instant_ms)
            .ok_or(EvaluationError::InvalidInstant)?;
        if instant_ms % OCCURRENCE_PRECISION_MS != 0 {
            return Err(EvaluationError::NotMinuteAligned);
        }
        Ok(Self { instant_ms })
    }

    pub const fn instant_ms(self) -> i64 {
        self.instant_ms
    }

    /// Classify this slot against a caller-supplied clock reading.
    ///
    /// Lateness within the occurrence's minute remains due. At the following
    /// minute it is missed. A clock rollback naturally returns `Upcoming`;
    /// this value has no hidden monotonic state.
    pub fn timing_at(self, now_ms: i64) -> OccurrenceTiming {
        if now_ms < self.instant_ms {
            OccurrenceTiming::Upcoming
        } else if now_ms.saturating_sub(self.instant_ms) < OCCURRENCE_PRECISION_MS {
            OccurrenceTiming::Due
        } else {
            OccurrenceTiming::Missed
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OccurrenceTiming {
    Upcoming,
    Due,
    Missed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluationError {
    InvalidInstant,
    NotMinuteAligned,
    Zone(ZoneError),
    ExplorerExhausted,
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInstant => formatter
                .write_str("the reference epoch millisecond is outside the supported calendar"),
            Self::NotMinuteAligned => {
                formatter.write_str("a recurrence occurrence must be aligned to a whole minute")
            }
            Self::Zone(error) => write!(formatter, "the recurrence timezone is invalid: {error:?}"),
            Self::ExplorerExhausted => formatter
                .write_str("the cron explorer did not advance beyond the DST lookback window"),
        }
    }
}

impl std::error::Error for EvaluationError {}

fn next_after<R: ZoneRules + ?Sized>(
    cron: &CronExpression,
    reference_ms: i64,
    rules: &R,
) -> Result<Option<Occurrence>, EvaluationError> {
    DateTime::<Utc>::from_timestamp_millis(reference_ms).ok_or(EvaluationError::InvalidInstant)?;
    let reference_wall = rules.wall_of(reference_ms);
    let search_wall = reference_wall
        .checked_sub_signed(TimeDelta::hours(DST_LOOKBACK_HOURS))
        .ok_or(EvaluationError::InvalidInstant)?;
    let search_from = search_wall.and_utc();

    // Looking back is required for spring gaps: at 03:00 local, a scheduled
    // 02:30 has resolved to 03:30 and is still in the future. The bounded
    // dialect's densest rule is every minute, so two hours needs about 120
    // candidates; the cap turns a dependency regression into a named error.
    let schedule = cron.schedule();
    let mut candidates = schedule.after(&search_from);
    for _ in 0..MAX_LOOKBACK_CANDIDATES {
        let Some(candidate) = candidates.next() else {
            return Ok(None);
        };
        let instant_ms = rules.instant_of(candidate.naive_utc());
        if instant_ms > reference_ms {
            return Occurrence::from_instant_ms(instant_ms).map(Some);
        }
    }

    Err(EvaluationError::ExplorerExhausted)
}
