// SPDX-License-Identifier: MPL-2.0
//! Selected owner observations, never execution or destination authority.
use super::*;
use koru_delegate_schedule::{RecurrenceRule, ScheduleState};

pub fn validate_routine_detail_request(
    bot: BotId,
    schedule: ScheduleId,
    limit: u8,
) -> Result<(), BotError> {
    validate_routine_list_request(bot, Some(schedule), limit)
}
pub fn validate_routine_detail_bytes<T: Serialize>(value: &T) -> Result<(), BotError> {
    bounded(value, BOT_ROUTINE_LIST_BYTES).map_err(|_| BotError::ContextTooLarge)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineDetail {
    pub bot_id: BotId,
    pub schedule_id: ScheduleId,
    #[serde(deserialize_with = "positive_revision")]
    pub bot_revision: u64,
    pub observed_at_ms: i64,
    pub creation: BotRoutineCreationObservation,
    /// The grant the Routine runs under now: the one it was set up with
    /// until its access is edited, the edited one after.
    pub access: BotRoutineGrantSummary,
    /// The original grant, frozen at creation.
    pub access_at_setup: BotRoutineGrantSummary,
    pub configured_model: BotRoutineModelObservation,
    pub destination: RoutineDestinationObservation,
    pub live: RoutineDetailLive,
    pub readiness: RoutineReadinessObservation,
    pub local_history: RoutineLocalHistoryObservation,
    /// What each past minute became, newest first: it ran, it was missed, or
    /// it was skipped because the Bot was busy. Read from the Schedule
    /// document for this Routine only. Nothing is caught up.
    pub occurrences: RoutineOccurrenceHistoryObservation,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineDestinationObservation {
    Current {
        home: WorkspaceId,
        working_folder: BotWorkingFolder,
    },
    Unavailable {},
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineDetailLive {
    Current {
        name: ScheduleName,
        #[serde(deserialize_with = "definition_revision")]
        definition_revision: u32,
        effective_at_ms: i64,
        recurrence: RecurrenceRule,
        #[serde(with = "super::listing::state")]
        #[cfg_attr(
            feature = "schema",
            schemars(with = "super::listing::state::StrictState")
        )]
        state: ScheduleState,
        owner_zone: RoutineOwnerZoneObservation,
        next: RoutineNextObservation,
    },
    NotLive {},
    Unavailable {},
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "zone", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineOwnerZoneObservation {
    Fixed {
        #[serde(deserialize_with = "iana")]
        iana: String,
    },
    OwnerLocal {},
    /// A floating rule whose owner is this computer: the time zone this
    /// computer is in now. Never a viewer's zone for a Routine owned
    /// elsewhere.
    ThisComputer {
        #[serde(deserialize_with = "iana")]
        iana: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "next", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineNextObservation {
    Exact { at_ms: i64 },
    OwnerLocal {},
    Inactive {},
    Unavailable {},
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "readiness", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineReadinessObservation {
    Blocked {
        #[serde(deserialize_with = "reasons")]
        reasons: Vec<RoutineReadinessReason>,
    },
    NotAssessed {},
    Unavailable {},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineReadinessReason {
    BotPaused,
    BotArchived,
    CreationNotBound,
    SchedulePaused,
    HandoffPending,
    ScheduleRetired,
    ScheduleNotLive,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineLocalHistoryObservation {
    Available {
        #[serde(deserialize_with = "history")]
        #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
        items: Vec<BotRoutineLocalConversation>,
        next_before_occurrence_ms: Option<i64>,
    },
    Unavailable {},
}

/// One page of a Routine's past minutes, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineOccurrenceHistoryObservation {
    Available {
        #[serde(deserialize_with = "occurrence_lines")]
        #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
        items: Vec<RoutineOccurrenceLine>,
        next_before_occurrence_ms: Option<i64>,
    },
    Unavailable {},
}
/// One past minute of a Routine. `ran` means its first turn was
/// acknowledged, not that a result was saved. `test` marks a minute a person
/// started with Test. `conversation` is present when this computer holds the
/// conversation of that minute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceLine {
    pub occurrence_ms: i64,
    pub outcome: RoutineOccurrenceOutcome,
    pub test: bool,
    pub recorded_at_ms: i64,
    pub conversation: Option<BotRoutineLocalConversation>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineOccurrenceOutcome {
    Ran,
    Missed,
    SkippedBusy,
}
impl From<ScheduleFiringOutcome> for RoutineOccurrenceOutcome {
    fn from(outcome: ScheduleFiringOutcome) -> Self {
        match outcome {
            ScheduleFiringOutcome::Fired => Self::Ran,
            ScheduleFiringOutcome::SkippedMissed => Self::Missed,
            ScheduleFiringOutcome::SkippedBusy => Self::SkippedBusy,
        }
    }
}

/// The most history lines one detail read returns.
pub const BOT_ROUTINE_HISTORY_LIMIT: usize = 16;

/// Trusted in-process page of a Routine's firings, read from its Schedule
/// document off the writer; no serde/token constructor. The lines are newest
/// first, at most the requested limit, all below the requested cursor.
#[derive(Clone, PartialEq, Eq)]
pub enum RoutineFiringsObservation {
    Available {
        lines: Vec<RoutineFiringLine>,
        next_before_occurrence_ms: Option<i64>,
    },
    /// The firing list did not decode, or was not read.
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutineFiringLine {
    pub occurrence_ms: i64,
    pub outcome: RoutineOccurrenceOutcome,
    pub recorded_at_ms: i64,
}
impl RoutineFiringsObservation {
    /// One page of a decoded firing list: the entries below `before`, the
    /// newest `limit` of them, and the cursor of the next page. The cost is
    /// linear in the list; nothing bounds the list itself.
    pub fn page<I>(firings: I, before: Option<i64>, limit: u8) -> Self
    where
        I: IntoIterator<Item = RoutineFiringLine>,
    {
        let limit = usize::from(limit).clamp(1, BOT_ROUTINE_HISTORY_LIMIT);
        let mut lines: Vec<RoutineFiringLine> = firings
            .into_iter()
            .filter(|line| before.is_none_or(|before| line.occurrence_ms < before))
            .collect();
        lines.sort_by_key(|line| std::cmp::Reverse(line.occurrence_ms));
        let more = lines.len() > limit;
        lines.truncate(limit);
        Self::Available {
            next_before_occurrence_ms: more
                .then(|| lines.last().map(|line| line.occurrence_ms))
                .flatten(),
            lines,
        }
    }
    /// The public history: each line joined, by its minute, with the local
    /// conversation of that minute and whether that minute was a Test. A
    /// firing from another computer has no local row and no link.
    pub fn history(
        &self,
        local: &[(BotRoutineLocalConversation, bool)],
    ) -> RoutineOccurrenceHistoryObservation {
        let Self::Available {
            lines,
            next_before_occurrence_ms,
        } = self
        else {
            return RoutineOccurrenceHistoryObservation::Unavailable {};
        };
        if lines.len() > BOT_ROUTINE_HISTORY_LIMIT {
            return RoutineOccurrenceHistoryObservation::Unavailable {};
        }
        RoutineOccurrenceHistoryObservation::Available {
            items: lines
                .iter()
                .map(|line| {
                    let row = local
                        .iter()
                        .find(|(row, _)| row.occurrence_ms == line.occurrence_ms);
                    RoutineOccurrenceLine {
                        occurrence_ms: line.occurrence_ms,
                        outcome: line.outcome,
                        test: row.is_some_and(|(_, test)| *test),
                        recorded_at_ms: line.recorded_at_ms,
                        // Only a minute that ran has a conversation to open.
                        conversation: row
                            .filter(|_| line.outcome == RoutineOccurrenceOutcome::Ran)
                            .map(|(row, _)| row.clone()),
                    }
                })
                .collect(),
            next_before_occurrence_ms: *next_before_occurrence_ms,
        }
    }
}
impl std::fmt::Debug for RoutineFiringsObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineFiringsObservation([REDACTED])")
    }
}

/// This computer, as the owner of a floating rule: its device and the time
/// zone it is in now. Detected by the caller on every read and never cached,
/// so a system zone change is seen by the next read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineThisComputer {
    pub device: DeviceId,
    pub zone: koru_delegate_schedule::CanonicalZone,
}

/// A strict flattened envelope closes only this new BotResponse variant. The
/// ordinary response enum's historical decoding remains unchanged.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineDetailEnvelope {
    pub detail: BotRoutineDetail,
}
impl<'de> Deserialize<'de> for BotRoutineDetailEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Envelope;
        impl<'de> serde::de::Visitor<'de> for Envelope {
            type Value = BotRoutineDetailEnvelope;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("one Routine detail field")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut detail = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "detail" {
                        return Err(serde::de::Error::unknown_field(&key, &["detail"]));
                    }
                    if detail.is_some() {
                        return Err(serde::de::Error::duplicate_field("detail"));
                    }
                    detail = Some(map.next_value()?);
                }
                Ok(BotRoutineDetailEnvelope {
                    detail: detail.ok_or_else(|| serde::de::Error::missing_field("detail"))?,
                })
            }
        }
        d.deserialize_map(Envelope)
    }
}

/// Trusted in-process Store observation; no serde/token constructor.
#[derive(Clone, PartialEq, Eq)]
pub enum RoutineDetailObservation {
    Current {
        name: ScheduleName,
        definition_revision: u32,
        effective_at_ms: i64,
        recurrence: RecurrenceRule,
        state: ScheduleState,
    },
    NotLive,
    Unavailable,
}
impl RoutineDetailObservation {
    /// The public observation with no knowledge of this computer's zone: a
    /// floating rule answers `owner_local`.
    pub fn live(self, observed_at_ms: i64) -> RoutineDetailLive {
        self.live_on(observed_at_ms, None)
    }
    /// The same, on the computer `this` describes. A floating rule whose
    /// current owner is this computer is evaluated in this computer's zone,
    /// for display only: the delegate's own evaluation decides when a
    /// Routine fires. Every other floating case stays `owner_local`; the
    /// viewer's zone is never substituted for an owner elsewhere.
    pub fn live_on(
        self,
        observed_at_ms: i64,
        this: Option<&RoutineThisComputer>,
    ) -> RoutineDetailLive {
        match self {
            Self::NotLive => RoutineDetailLive::NotLive {},
            Self::Unavailable => RoutineDetailLive::Unavailable {},
            Self::Current {
                name,
                definition_revision,
                effective_at_ms,
                recurrence,
                state,
            } => {
                let (owner_zone, next) = routine_zone_and_next(
                    &recurrence,
                    &state,
                    effective_at_ms,
                    observed_at_ms,
                    this,
                );
                RoutineDetailLive::Current {
                    name,
                    definition_revision,
                    effective_at_ms,
                    recurrence,
                    state,
                    owner_zone,
                    next,
                }
            }
        }
    }
}
/// The zone a rule is evaluated in and its next time, from one observation.
/// No clock and no system zone is read here.
pub fn routine_zone_and_next(
    recurrence: &RecurrenceRule,
    state: &ScheduleState,
    effective_at_ms: i64,
    observed_at_ms: i64,
    this: Option<&RoutineThisComputer>,
) -> (RoutineOwnerZoneObservation, RoutineNextObservation) {
    // The document's current owner, after handoff reduction. A pending
    // handoff or a retired Schedule has no owner this computer may speak for.
    let owner = match state {
        ScheduleState::Active { owner } | ScheduleState::Paused { owner, .. } => Some(*owner),
        ScheduleState::HandoffPending { .. } | ScheduleState::Retired => None,
    };
    let here = this.filter(|this| owner == Some(this.device));
    let zone = match recurrence {
        RecurrenceRule::Zoned { zone, .. } => Some(zone),
        RecurrenceRule::Floating { .. } => here.map(|this| &this.zone),
    };
    let owner_zone = match (recurrence.zone_name(), here) {
        (Some(iana), _) => RoutineOwnerZoneObservation::Fixed { iana: iana.into() },
        (None, Some(this)) => RoutineOwnerZoneObservation::ThisComputer {
            iana: this.zone.as_str().into(),
        },
        (None, None) => RoutineOwnerZoneObservation::OwnerLocal {},
    };
    let next = if !matches!(state, ScheduleState::Active { .. }) {
        RoutineNextObservation::Inactive {}
    } else {
        match zone {
            None => RoutineNextObservation::OwnerLocal {},
            Some(zone) => match koru_delegate_schedule::next_zoned_after_at_least(
                recurrence.cron(),
                zone,
                observed_at_ms,
                effective_at_ms,
            ) {
                Ok(Some(next)) => RoutineNextObservation::Exact {
                    at_ms: next.instant_ms(),
                },
                _ => RoutineNextObservation::Unavailable {},
            },
        }
    };
    (owner_zone, next)
}
pub fn routine_readiness(
    paused: bool,
    archived: bool,
    bound: bool,
    live: &RoutineDetailLive,
) -> RoutineReadinessObservation {
    let mut reasons = Vec::new();
    if paused {
        reasons.push(RoutineReadinessReason::BotPaused);
    }
    if archived {
        reasons.push(RoutineReadinessReason::BotArchived);
    }
    if !bound {
        reasons.push(RoutineReadinessReason::CreationNotBound);
    }
    match live {
        RoutineDetailLive::Current { state, .. } => match state {
            ScheduleState::Paused { .. } => reasons.push(RoutineReadinessReason::SchedulePaused),
            ScheduleState::HandoffPending { .. } => {
                reasons.push(RoutineReadinessReason::HandoffPending)
            }
            ScheduleState::Retired => reasons.push(RoutineReadinessReason::ScheduleRetired),
            ScheduleState::Active { .. } => {}
        },
        RoutineDetailLive::NotLive {} => reasons.push(RoutineReadinessReason::ScheduleNotLive),
        _ => {}
    }
    if reasons.is_empty() {
        if matches!(live, RoutineDetailLive::Unavailable {}) {
            RoutineReadinessObservation::Unavailable {}
        } else {
            RoutineReadinessObservation::NotAssessed {}
        }
    } else {
        RoutineReadinessObservation::Blocked { reasons }
    }
}
fn positive_revision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = u64::deserialize(d)?;
    if revision(value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("invalid revision"))
    }
}
fn definition_revision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let value = u32::deserialize(d)?;
    if value > 0 {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("invalid definition revision"))
    }
}
fn iana<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    koru_delegate_schedule::CanonicalZone::parse(&value)
        .map(|v| v.as_str().to_owned())
        .map_err(serde::de::Error::custom)
}
fn history<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<BotRoutineLocalConversation>, D::Error> {
    limited(d, 16)
}
fn occurrence_lines<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<RoutineOccurrenceLine>, D::Error> {
    limited(d, BOT_ROUTINE_HISTORY_LIMIT)
}
fn reasons<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<RoutineReadinessReason>, D::Error> {
    let values = limited(d, 7)?;
    if values.is_empty() || values.windows(2).any(|v| v[0] >= v[1]) {
        Err(serde::de::Error::custom("invalid blocker order"))
    } else {
        Ok(values)
    }
}
fn limited<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
    max: usize,
) -> Result<Vec<T>, D::Error> {
    struct Items<T>(usize, std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Items<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded observation list")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Vec<T>, A::Error> {
            if a.size_hint().is_some_and(|n| n > self.0) {
                return Err(serde::de::Error::custom("observation list limit"));
            }
            let mut result = Vec::with_capacity(self.0);
            while let Some(value) = a.next_element()? {
                if result.len() == self.0 {
                    return Err(serde::de::Error::custom("observation list limit"));
                }
                result.push(value);
            }
            Ok(result)
        }
    }
    d.deserialize_seq(Items(max, std::marker::PhantomData))
}
macro_rules! redact {($($ty:ty),*)=>{$(impl std::fmt::Debug for $ty {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(concat!(stringify!($ty),"([REDACTED])"))}})*};}
redact!(
    BotRoutineDetail,
    BotRoutineDetailEnvelope,
    RoutineDestinationObservation,
    RoutineDetailLive,
    RoutineOwnerZoneObservation,
    RoutineDetailObservation
);

#[cfg(test)]
mod tests;
