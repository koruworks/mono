// SPDX-License-Identifier: MPL-2.0

//! One `koru-schedule` document: coherent definition, append-only ownership,
//! retirement, and occurrence history.

use std::fmt;

mod occurrence;
mod seed;

use koru_delegate_core::{
    DelegationRequest, DeviceId, HandoffId, RoutineClaimFacts, RoutineDefinitionIdentity,
    RoutinePrepared, RoutineRepresentativeMiss, RoutineSlot, RunContextItem, RunId, ScheduleId,
    WorkspaceId,
};
use koru_delegate_schedule::{
    DelegationTemplate, HandoffEvent, HandoffState, ScheduleDefinition, ScheduleFiring,
    ScheduleFiringOutcome, ScheduleHeader, ScheduleLifecycle, ScheduleName, ScheduleState,
};
use koru_store_core::version::Compatibility;
use loro::{ExportMode, LoroDoc, LoroList, LoroMap, LoroValue, ValueOrContainer, VersionVector};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION, ORDINARY_SCHEDULE_DOCUMENT_SCHEMA_VERSION,
    SCHEDULE_DOCUMENT_SCHEMA_VERSION,
};

const ROOT: &str = "schedule";
const K_SCHEMA: &str = "schema";
const K_ID: &str = "id";
const K_CREATED: &str = "created_at_ms";
const K_OWNER: &str = "initial_owner";
const K_NAME: &str = "name";
const K_DEFINITION: &str = "definition";
const K_RETIRED: &str = "retired";
const HANDOFFS: &str = "handoffs";
const FIRINGS: &str = "firings";

type Result<T> = std::result::Result<T, ScheduleDocumentError>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScheduleWrite {
    Applied,
    AlreadyApplied,
}

/// One exact occurrence action proposed to [`ScheduleDocument`]. A Run plan
/// supplies P2.4's pins but no request fields; the document derives those from
/// its authoritative template.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScheduleClaimPlan {
    BotRoutine {
        prepared: RoutinePrepared,
        representative_miss: Option<RoutineRepresentativeMiss>,
    },
    Run {
        occurrence_ms: i64,
        run: RunId,
        pinned_context: Vec<RunContextItem>,
    },
    Skip {
        occurrence_ms: i64,
        run: RunId,
        outcome: ScheduleFiringOutcome,
        busy_holder: Option<RunId>,
    },
}

enum BoundScheduleClaim {
    BotRoutine {
        prepared: RoutinePrepared,
        representative_miss: Option<RoutineRepresentativeMiss>,
    },
    Run {
        occurrence_ms: i64,
        request: DelegationRequest,
    },
    Skip {
        occurrence_ms: i64,
        run: RunId,
        outcome: ScheduleFiringOutcome,
        busy_holder: Option<RunId>,
    },
}

/// The complete launch authorized by consuming a schedule claim proof.
///
/// `Debug` deliberately omits the request because it contains the objective.
pub struct ScheduleRunClaim {
    pub schedule: ScheduleId,
    pub revision: u32,
    pub workspace: WorkspaceId,
    pub occurrence_ms: i64,
    pub request: DelegationRequest,
}

impl fmt::Debug for ScheduleRunClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScheduleRunClaim")
            .field("schedule", &self.schedule)
            .field("revision", &self.revision)
            .field("workspace", &self.workspace)
            .field("occurrence_ms", &self.occurrence_ms)
            .field("run", &self.request.run)
            .finish_non_exhaustive()
    }
}

/// The complete, already-settled skip authorized by consuming a claim proof.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduleSkipClaim {
    pub schedule: ScheduleId,
    pub revision: u32,
    pub workspace: WorkspaceId,
    pub occurrence_ms: i64,
    pub run: RunId,
    pub outcome: ScheduleFiringOutcome,
    pub busy_holder: Option<RunId>,
}

/// Single-use proof bound to one exact launch or skip plan. It retains a
/// handle to the live Loro document and revalidates it when consumed, so a
/// pause, retirement, edit, confirmed handoff, or newer schema invalidates a
/// token minted before that change.
pub struct ScheduleClaimAuthority {
    doc: LoroDoc,
    compatibility: Compatibility,
    schedule: ScheduleId,
    revision: u32,
    owner: DeviceId,
    epoch: Option<HandoffId>,
    definition: ScheduleDefinition,
    workspace: WorkspaceId,
    binding: BoundScheduleClaim,
}

impl fmt::Debug for ScheduleClaimAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScheduleClaimAuthority")
            .field("schedule", &self.schedule)
            .field("revision", &self.revision)
            .field("owner", &self.owner)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl ScheduleClaimAuthority {
    fn validate_current(&self) -> Result<()> {
        let current = ScheduleDocument {
            doc: self.doc.clone(),
            compatibility: self.compatibility,
        };
        current.writable()?;
        current.firings()?;
        let header = current.header()?;
        let handoff = current.handoff_state()?;
        if !current.may_claim(self.owner)?
            || header.schedule != self.schedule
            || header.definition != self.definition
            || handoff.owner != self.owner
            || handoff.epoch != self.epoch
        {
            return Err(ScheduleDocumentError::StaleClaimAuthority);
        }
        Ok(())
    }

    pub fn into_run(self) -> Result<ScheduleRunClaim> {
        if self.definition.routine.is_some() {
            return Err(ScheduleDocumentError::RoutineRequiresBotClaim);
        }
        self.validate_current()?;
        match self.binding {
            BoundScheduleClaim::Run {
                occurrence_ms,
                mut request,
            } => {
                request.origin = koru_delegate_core::RunOrigin::Scheduled {
                    schedule: self.schedule,
                    occurrence_ms,
                    revision: self.revision,
                };
                Ok(ScheduleRunClaim {
                    schedule: self.schedule,
                    revision: self.revision,
                    workspace: self.workspace,
                    occurrence_ms,
                    request,
                })
            }
            _ => Err(ScheduleDocumentError::ClaimBindingMismatch),
        }
    }

    pub fn into_skip(self) -> Result<ScheduleSkipClaim> {
        if self.definition.routine.is_some() {
            return Err(ScheduleDocumentError::RoutineRequiresBotClaim);
        }
        self.validate_current()?;
        match self.binding {
            BoundScheduleClaim::Skip {
                occurrence_ms,
                run,
                outcome,
                busy_holder,
            } => Ok(ScheduleSkipClaim {
                schedule: self.schedule,
                revision: self.revision,
                workspace: self.workspace,
                occurrence_ms,
                run,
                outcome,
                busy_holder,
            }),
            _ => Err(ScheduleDocumentError::ClaimBindingMismatch),
        }
    }

    /// Consume actual live authority. Document versions are not compared:
    /// unrelated name/history changes do not alter the authority state above.
    pub fn into_bot_routine(self) -> Result<RoutineClaimFacts> {
        self.validate_current()?;
        let association = self
            .definition
            .routine
            .as_ref()
            .ok_or(ScheduleDocumentError::ClaimBindingMismatch)?;
        let BoundScheduleClaim::BotRoutine {
            prepared,
            representative_miss,
        } = self.binding
        else {
            return Err(ScheduleDocumentError::ClaimBindingMismatch);
        };
        let slot = RoutineSlot {
            owner: association.owner,
            bot: association.bot,
            schedule: self.schedule,
            occurrence_ms: prepared.slot.occurrence_ms,
        };
        let definition = routine_identity(&self.definition, self.workspace, self.epoch)?;
        RoutineClaimFacts::from_consumed_authority(
            prepared,
            slot,
            definition,
            self.workspace,
            association.creation_request,
            representative_miss,
        )
        .ok_or(ScheduleDocumentError::ClaimBindingMismatch)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScheduleDocumentError {
    NotASchedule,
    AlreadyInitialized,
    ReadOnly { found: i64, understood: i64 },
    Malformed(&'static str),
    RevisionZero,
    RevisionConflict { current: u32, offered: u32 },
    NameConflict,
    EffectiveTimeBeforeCreation,
    EffectiveTimeDidNotAdvance,
    RevisionExhausted,
    NotOwner,
    Retired,
    HandoffPending,
    LifecycleConflict,
    ClaimBindingMismatch,
    RoutineAssociationChanged,
    RoutineTemplateChanged,
    RoutineRequiresBotClaim,
    StaleClaimAuthority,
    UnknownHandoff,
    HandoffConflict,
    FiringConflict,
    Crdt(String),
}

impl fmt::Display for ScheduleDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly { found, understood } => {
                write!(f, "schedule schema {found} is newer than {understood}")
            }
            Self::RevisionConflict { current, offered } => {
                write!(f, "schedule revision {offered} does not follow {current}")
            }
            Self::NameConflict => f.write_str("schedule name changed since it was observed"),
            Self::EffectiveTimeBeforeCreation => {
                f.write_str("schedule effective time precedes creation")
            }
            Self::EffectiveTimeDidNotAdvance => {
                f.write_str("schedule effective time did not advance")
            }
            Self::RevisionExhausted => f.write_str("schedule revision is exhausted"),
            Self::Malformed(part) => write!(f, "malformed schedule {part}"),
            Self::Crdt(error) => write!(f, "schedule CRDT: {error}"),
            Self::NotASchedule => f.write_str("not a schedule document"),
            Self::AlreadyInitialized => f.write_str("schedule is already initialized"),
            Self::RevisionZero => f.write_str("schedule revision must be nonzero"),
            Self::NotOwner => f.write_str("device is not the effective schedule owner"),
            Self::Retired => f.write_str("schedule is retired"),
            Self::HandoffPending => f.write_str("schedule ownership handoff is pending"),
            Self::LifecycleConflict => {
                f.write_str("schedule lifecycle cannot make the requested transition")
            }
            Self::ClaimBindingMismatch => f.write_str("schedule claim binding does not match"),
            Self::RoutineAssociationChanged => {
                f.write_str("a schedule's Bot association cannot be changed")
            }
            Self::RoutineTemplateChanged => {
                f.write_str("a Bot routine inherits its execution settings and access")
            }
            Self::RoutineRequiresBotClaim => {
                f.write_str("a Bot routine requires its Bot execution owner")
            }
            Self::StaleClaimAuthority => f.write_str("schedule claim authority is stale"),
            Self::UnknownHandoff => f.write_str("handoff request is unknown"),
            Self::HandoffConflict => f.write_str("handoff id already names another fact"),
            Self::FiringConflict => f.write_str("occurrence already has another firing record"),
        }
    }
}

impl std::error::Error for ScheduleDocumentError {}

impl From<loro::LoroError> for ScheduleDocumentError {
    fn from(error: loro::LoroError) -> Self {
        Self::Crdt(error.to_string())
    }
}

#[derive(Serialize, Deserialize)]
struct Retirement {
    by: DeviceId,
    revision: u32,
    at_ms: i64,
}

pub struct ScheduleDocument {
    doc: LoroDoc,
    compatibility: Compatibility,
}

impl ScheduleDocument {
    pub fn create(header: &ScheduleHeader) -> Result<Self> {
        let name = ScheduleName::from_objective(&header.definition.template.objective);
        Self::initialize_named(LoroDoc::new(), header, &name)
    }

    pub fn initialize(doc: LoroDoc, header: &ScheduleHeader) -> Result<Self> {
        let name = ScheduleName::from_objective(&header.definition.template.objective);
        Self::initialize_named(doc, header, &name)
    }

    pub fn create_named(header: &ScheduleHeader, name: &ScheduleName) -> Result<Self> {
        Self::initialize_named(LoroDoc::new(), header, name)
    }

    pub fn initialize_named(
        doc: LoroDoc,
        header: &ScheduleHeader,
        name: &ScheduleName,
    ) -> Result<Self> {
        if header.definition.revision == 0 {
            return Err(ScheduleDocumentError::RevisionZero);
        }
        if header.definition.effective_at_ms < header.created_at_ms {
            return Err(ScheduleDocumentError::EffectiveTimeBeforeCreation);
        }
        let schema = if let Some(routine) = &header.definition.routine {
            if !routine.valid() || routine.owner.device != header.initial_owner {
                return Err(ScheduleDocumentError::Malformed("routine"));
            }
            if !matches!(
                header.definition.lifecycle,
                ScheduleLifecycle::Paused { .. }
            ) {
                return Err(ScheduleDocumentError::LifecycleConflict);
            }
            BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION
        } else {
            ORDINARY_SCHEDULE_DOCUMENT_SCHEMA_VERSION
        };
        let root = doc.get_map(ROOT);
        if root.get(K_SCHEMA).is_some() {
            return Err(ScheduleDocumentError::AlreadyInitialized);
        }
        root.insert(K_SCHEMA, schema)?;
        root.insert(K_ID, encode(&header.schedule, K_ID)?)?;
        root.insert(K_CREATED, header.created_at_ms)?;
        root.insert(K_OWNER, encode(&header.initial_owner, K_OWNER)?)?;
        root.insert(K_NAME, encode(name, K_NAME)?)?;
        root.insert(K_DEFINITION, encode(&header.definition, K_DEFINITION)?)?;
        doc.get_list(HANDOFFS);
        doc.get_list(FIRINGS);
        doc.commit();
        Ok(Self {
            doc,
            compatibility: Compatibility::Writable,
        })
    }

    pub fn adopt(doc: LoroDoc) -> Result<Self> {
        let found = match doc.get_map(ROOT).get(K_SCHEMA) {
            Some(ValueOrContainer::Value(LoroValue::I64(found))) => found,
            _ => return Err(ScheduleDocumentError::NotASchedule),
        };
        Ok(Self {
            doc,
            compatibility: Compatibility::against(Some(found), SCHEDULE_DOCUMENT_SCHEMA_VERSION),
        })
    }

    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        Self::adopt(doc)
    }

    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }
    pub fn compatibility(&self) -> Compatibility {
        match self.root().get(K_SCHEMA) {
            Some(ValueOrContainer::Value(LoroValue::I64(found))) => {
                Compatibility::against(Some(found), SCHEDULE_DOCUMENT_SCHEMA_VERSION)
            }
            _ => self.compatibility,
        }
    }
    pub fn is_writable(&self) -> bool {
        matches!(self.compatibility(), Compatibility::Writable) && self.header().is_ok()
    }
    pub fn version(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    fn root(&self) -> LoroMap {
        self.doc.get_map(ROOT)
    }
    fn handoff_list(&self) -> LoroList {
        self.doc.get_list(HANDOFFS)
    }
    fn firing_list(&self) -> LoroList {
        self.doc.get_list(FIRINGS)
    }

    fn writable(&self) -> Result<()> {
        match self.compatibility() {
            Compatibility::Writable => self.header().map(|_| ()),
            Compatibility::TooNew { found, understood } => {
                Err(ScheduleDocumentError::ReadOnly { found, understood })
            }
        }
    }

    pub fn header(&self) -> Result<ScheduleHeader> {
        let root = self.root();
        let schema = value(&root, K_SCHEMA)?;
        let definition: serde_json::Value = value(&root, K_DEFINITION)?.into();
        // Only the new coherent definition envelope is closed. Existing nested
        // template/recurrence decoders and all schema-1 decoding stay unchanged.
        if matches!(schema, LoroValue::I64(BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION))
            && definition.as_object().is_none_or(|fields| {
                fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "revision"
                            | "effective_at_ms"
                            | "template"
                            | "recurrence"
                            | "lifecycle"
                            | "routine"
                    )
                })
            })
        {
            return Err(ScheduleDocumentError::Malformed(K_DEFINITION));
        }
        let header = ScheduleHeader {
            schedule: decode(value(&root, K_ID)?, K_ID)?,
            created_at_ms: match value(&root, K_CREATED)? {
                LoroValue::I64(at) => at,
                _ => return Err(ScheduleDocumentError::Malformed(K_CREATED)),
            },
            initial_owner: decode(value(&root, K_OWNER)?, K_OWNER)?,
            definition: serde_json::from_value(definition)
                .map_err(|_| ScheduleDocumentError::Malformed(K_DEFINITION))?,
        };
        match (schema, &header.definition.routine) {
            (LoroValue::I64(ORDINARY_SCHEDULE_DOCUMENT_SCHEMA_VERSION), None) => {}
            (LoroValue::I64(BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION), Some(routine))
                if routine.valid() && routine.owner.device == header.initial_owner => {}
            (LoroValue::I64(found), _) if found > SCHEDULE_DOCUMENT_SCHEMA_VERSION => {
                return Err(ScheduleDocumentError::ReadOnly {
                    found,
                    understood: SCHEDULE_DOCUMENT_SCHEMA_VERSION,
                });
            }
            _ => return Err(ScheduleDocumentError::Malformed("routine_schema")),
        }
        Ok(header)
    }

    /// The independently mutable display name. Schema-1 documents written by
    /// the earlier unreleased build derive the same bounded first-line label.
    pub fn name(&self) -> Result<ScheduleName> {
        let root = self.root();
        match root.get(K_NAME) {
            Some(ValueOrContainer::Value(value)) => decode(value, K_NAME),
            Some(ValueOrContainer::Container(_)) => Err(ScheduleDocumentError::Malformed(K_NAME)),
            None => Ok(ScheduleName::from_objective(
                &self.header()?.definition.template.objective,
            )),
        }
    }

    pub fn handoffs(&self) -> Result<Vec<HandoffEvent>> {
        decode_list(self.handoff_list(), HANDOFFS)
    }

    pub fn firings(&self) -> Result<Vec<ScheduleFiring>> {
        let mut firings = decode_list(self.firing_list(), FIRINGS)?;
        firings.sort_by_key(|firing: &ScheduleFiring| firing.occurrence_ms);
        let mut canonical: Vec<ScheduleFiring> = Vec::with_capacity(firings.len());
        for firing in firings {
            match canonical.last() {
                Some(known) if known == &firing => {}
                Some(known) if known.occurrence_ms == firing.occurrence_ms => {
                    return Err(ScheduleDocumentError::FiringConflict);
                }
                _ => canonical.push(firing),
            }
        }
        Ok(canonical)
    }

    pub fn handoff_state(&self) -> Result<HandoffState> {
        let header = self.header()?;
        Ok(HandoffState::reduce(
            header.initial_owner,
            &self.handoffs()?,
        ))
    }

    pub fn state(&self) -> Result<ScheduleState> {
        if self.root().get(K_RETIRED).is_some() {
            return Ok(ScheduleState::Retired);
        }
        let header = self.header()?;
        let handoff = self.handoff_state()?;
        if let ScheduleLifecycle::Paused { reason } = header.definition.lifecycle {
            return Ok(ScheduleState::Paused {
                owner: handoff.owner,
                reason,
            });
        }
        if !handoff.pending.is_empty() {
            return Ok(ScheduleState::HandoffPending {
                owner: handoff.owner,
                requests: handoff.pending,
            });
        }
        Ok(ScheduleState::Active {
            owner: handoff.owner,
        })
    }

    /// A pending request is visible but does not disable the old owner. Only
    /// an observed valid confirmation changes this answer.
    pub fn may_claim(&self, device: DeviceId) -> Result<bool> {
        if self.root().get(K_RETIRED).is_some() {
            return Ok(false);
        }
        let header = self.header()?;
        Ok(
            matches!(header.definition.lifecycle, ScheduleLifecycle::Active)
                && self.handoff_state()?.owner == device,
        )
    }

    pub fn claim_authority(
        &self,
        workspace: WorkspaceId,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> Result<Option<ScheduleClaimAuthority>> {
        self.writable()?;
        self.firings()?;
        let header = self.header()?;
        if header.definition.routine.is_some()
            && !matches!(plan, ScheduleClaimPlan::BotRoutine { .. })
        {
            return Err(ScheduleDocumentError::RoutineRequiresBotClaim);
        }
        if !self.may_claim(device)? {
            return Ok(None);
        }
        let handoff = self.handoff_state()?;
        let binding = match plan {
            ScheduleClaimPlan::BotRoutine {
                prepared,
                representative_miss,
            } => bind_routine(
                &header,
                workspace,
                device,
                handoff.epoch,
                prepared,
                representative_miss,
            )?,
            ordinary => bind_plan(&header.definition.template, device, ordinary)?,
        };
        Ok(Some(ScheduleClaimAuthority {
            doc: self.doc.clone(),
            compatibility: self.compatibility,
            schedule: header.schedule,
            revision: header.definition.revision,
            owner: device,
            epoch: handoff.epoch,
            definition: header.definition,
            workspace,
            binding,
        }))
    }

    pub fn update_definition(&self, by: DeviceId, definition: &ScheduleDefinition) -> Result<()> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        if self.handoff_state()?.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let current_definition = self.header()?.definition;
        preserve_routine(&current_definition, definition)?;
        let current = current_definition.revision;
        if definition.revision == 0 {
            return Err(ScheduleDocumentError::RevisionZero);
        }
        let expected = current
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        if definition.revision != expected {
            return Err(ScheduleDocumentError::RevisionConflict {
                current,
                offered: definition.revision,
            });
        }
        if definition.effective_at_ms <= current_definition.effective_at_ms {
            return Err(ScheduleDocumentError::EffectiveTimeDidNotAdvance);
        }
        self.root()
            .insert(K_DEFINITION, encode(definition, K_DEFINITION)?)?;
        self.doc.commit();
        Ok(())
    }

    /// Replace only the independently mutable display name. The observed
    /// value prevents a delayed client from overwriting a newer rename; an
    /// exact requested value is the successful replay of a lost acknowledgement.
    pub fn rename(
        &self,
        by: DeviceId,
        expected: &ScheduleName,
        name: &ScheduleName,
    ) -> Result<ScheduleWrite> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        if self.handoff_state()?.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let current = self.name()?;
        if &current == name {
            return Ok(ScheduleWrite::AlreadyApplied);
        }
        if &current != expected {
            return Err(ScheduleDocumentError::NameConflict);
        }
        self.root().insert(K_NAME, encode(name, K_NAME)?)?;
        self.doc.commit();
        Ok(ScheduleWrite::Applied)
    }

    /// Replace every person-editable field in one coherent revision.
    ///
    /// `expected_revision` and `expected_name` bind the form to the snapshot
    /// it displayed. An exact successor is a successful replay after a lost
    /// acknowledgement; any other successor refuses instead of combining two
    /// people's edits. Ownership and lifecycle remain document authority.
    pub fn edit(
        &self,
        by: DeviceId,
        expected_revision: u32,
        expected_name: &ScheduleName,
        name: &ScheduleName,
        definition: &ScheduleDefinition,
    ) -> Result<ScheduleWrite> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        let handoff = self.handoff_state()?;
        if !handoff.pending.is_empty() {
            return Err(ScheduleDocumentError::HandoffPending);
        }
        if handoff.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let current_name = self.name()?;
        let current_definition = self.header()?.definition;
        preserve_routine(&current_definition, definition)?;
        let successor = expected_revision
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        if current_definition.revision == successor
            && current_name == *name
            && current_definition == *definition
        {
            return Ok(ScheduleWrite::AlreadyApplied);
        }
        if current_definition.revision != expected_revision {
            return Err(ScheduleDocumentError::RevisionConflict {
                current: current_definition.revision,
                offered: successor,
            });
        }
        if current_name != *expected_name {
            return Err(ScheduleDocumentError::NameConflict);
        }
        if definition.revision != successor {
            return Err(ScheduleDocumentError::RevisionConflict {
                current: current_definition.revision,
                offered: definition.revision,
            });
        }
        if definition.effective_at_ms <= current_definition.effective_at_ms {
            return Err(ScheduleDocumentError::EffectiveTimeDidNotAdvance);
        }
        if definition.lifecycle != current_definition.lifecycle {
            return Err(ScheduleDocumentError::Malformed(K_DEFINITION));
        }
        let encoded_name = encode(name, K_NAME)?;
        let encoded_definition = encode(definition, K_DEFINITION)?;
        self.root().insert(K_NAME, encoded_name)?;
        self.root().insert(K_DEFINITION, encoded_definition)?;
        self.doc.commit();
        Ok(ScheduleWrite::Applied)
    }

    /// Change only whether future occurrences may be claimed. A replay of the
    /// exact successor is free; every other stale or incompatible transition
    /// is refused before the document changes.
    pub fn set_lifecycle(
        &self,
        by: DeviceId,
        expected_revision: u32,
        lifecycle: ScheduleLifecycle,
        effective_at_ms: i64,
    ) -> Result<ScheduleWrite> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        let handoff = self.handoff_state()?;
        if !handoff.pending.is_empty() {
            return Err(ScheduleDocumentError::HandoffPending);
        }
        if handoff.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let current = self.header()?.definition;
        let successor = expected_revision
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        if current.revision == successor && current.lifecycle == lifecycle {
            return Ok(ScheduleWrite::AlreadyApplied);
        }
        if current.revision != expected_revision {
            return Err(ScheduleDocumentError::RevisionConflict {
                current: current.revision,
                offered: successor,
            });
        }
        let compatible = matches!(
            (&current.lifecycle, &lifecycle),
            (
                ScheduleLifecycle::Active,
                ScheduleLifecycle::Paused {
                    reason: koru_delegate_schedule::SchedulePauseReason::Manual
                }
            ) | (ScheduleLifecycle::Paused { .. }, ScheduleLifecycle::Active)
        );
        if !compatible {
            return Err(ScheduleDocumentError::LifecycleConflict);
        }
        if effective_at_ms <= current.effective_at_ms {
            return Err(ScheduleDocumentError::EffectiveTimeDidNotAdvance);
        }
        let definition = ScheduleDefinition {
            revision: successor,
            effective_at_ms,
            template: current.template,
            recurrence: current.recurrence,
            lifecycle,
            routine: current.routine,
        };
        self.root()
            .insert(K_DEFINITION, encode(&definition, K_DEFINITION)?)?;
        self.doc.commit();
        Ok(ScheduleWrite::Applied)
    }

    pub fn request_handoff(
        &self,
        handoff: HandoffId,
        requested_by: DeviceId,
        to: DeviceId,
        at_ms: i64,
    ) -> Result<ScheduleWrite> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        if let Some(existing) = self.handoffs()?.into_iter().find(
            |event| matches!(event, HandoffEvent::Requested { handoff: id, .. } if *id == handoff),
        ) {
            return match existing {
                HandoffEvent::Requested {
                    requested_by: known_by,
                    to: known_to,
                    at_ms: known_at,
                    ..
                } if known_by == requested_by && known_to == to && known_at == at_ms => {
                    Ok(ScheduleWrite::AlreadyApplied)
                }
                _ => Err(ScheduleDocumentError::HandoffConflict),
            };
        }
        let state = self.handoff_state()?;
        let event = HandoffEvent::Requested {
            handoff,
            previous: state.epoch,
            requested_by,
            from: state.owner,
            to,
            at_ms,
        };
        let outcome = append_handoff_once(&self.handoff_list(), &event)?;
        if outcome == ScheduleWrite::Applied {
            self.doc.commit();
        }
        Ok(outcome)
    }

    pub fn confirm_handoff(
        &self,
        handoff: HandoffId,
        by: DeviceId,
        at_ms: i64,
    ) -> Result<ScheduleWrite> {
        self.writable()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        if let Some(existing) = self.handoffs()?.into_iter().find(
            |event| matches!(event, HandoffEvent::Confirmed { handoff: id, .. } if *id == handoff),
        ) {
            return match existing {
                HandoffEvent::Confirmed {
                    confirmed_by,
                    at_ms: known_at,
                    ..
                } if confirmed_by == by && known_at == at_ms => Ok(ScheduleWrite::AlreadyApplied),
                _ => Err(ScheduleDocumentError::HandoffConflict),
            };
        }
        let state = self.handoff_state()?;
        if state.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let known = self.handoffs()?.into_iter().any(|event| {
            matches!(event,
            HandoffEvent::Requested { handoff: id, previous, from, .. }
                if id == handoff && previous == state.epoch && from == state.owner)
        });
        if !known {
            return Err(ScheduleDocumentError::UnknownHandoff);
        }
        let event = HandoffEvent::Confirmed {
            handoff,
            confirmed_by: by,
            at_ms,
        };
        let outcome = append_handoff_once(&self.handoff_list(), &event)?;
        if outcome == ScheduleWrite::Applied {
            self.doc.commit();
        }
        Ok(outcome)
    }

    pub fn retire(&self, by: DeviceId, at_ms: i64) -> Result<ScheduleWrite> {
        self.writable()?;
        if let Some(retired) = self.root().get(K_RETIRED) {
            let retirement: Retirement = match retired {
                ValueOrContainer::Value(value) => decode(value, K_RETIRED)?,
                ValueOrContainer::Container(_) => {
                    return Err(ScheduleDocumentError::Malformed(K_RETIRED));
                }
            };
            return if retirement.by == by {
                Ok(ScheduleWrite::AlreadyApplied)
            } else {
                Err(ScheduleDocumentError::NotOwner)
            };
        }
        if self.handoff_state()?.owner != by {
            return Err(ScheduleDocumentError::NotOwner);
        }
        let retirement = Retirement {
            by,
            revision: self.header()?.definition.revision,
            at_ms,
        };
        self.root()
            .insert(K_RETIRED, encode(&retirement, K_RETIRED)?)?;
        self.doc.commit();
        Ok(ScheduleWrite::Applied)
    }

    pub fn append_firing(&self, firing: &ScheduleFiring) -> Result<ScheduleWrite> {
        self.writable()?;
        if firing.revision == 0 {
            return Err(ScheduleDocumentError::RevisionZero);
        }
        if (firing.outcome == ScheduleFiringOutcome::SkippedBusy) != firing.busy_holder.is_some() {
            return Err(ScheduleDocumentError::Malformed(FIRINGS));
        }
        if let Some(existing) = self
            .firings()?
            .into_iter()
            .find(|known| known.occurrence_ms == firing.occurrence_ms)
        {
            return if existing == *firing {
                Ok(ScheduleWrite::AlreadyApplied)
            } else {
                Err(ScheduleDocumentError::FiringConflict)
            };
        }
        self.firing_list().push(encode(firing, FIRINGS)?)?;
        self.doc.commit();
        Ok(ScheduleWrite::Applied)
    }

    /// Durable restart cursor: a missed outcome advances through when it was
    /// observed, while every other outcome advances through its slot.
    pub fn observed_through_ms(&self) -> Result<Option<i64>> {
        Ok(self
            .firings()?
            .into_iter()
            .map(|firing| {
                if firing.outcome == ScheduleFiringOutcome::SkippedMissed {
                    firing.at
                } else {
                    firing.occurrence_ms
                }
            })
            .max())
    }

    pub fn export_updates_since(&self, since: &VersionVector) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::updates(since))
            .map_err(|error| ScheduleDocumentError::Crdt(error.to_string()))
    }
    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|error| ScheduleDocumentError::Crdt(error.to_string()))
    }
    pub fn import(&self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        Ok(())
    }
}

fn value(map: &LoroMap, key: &'static str) -> Result<LoroValue> {
    match map.get(key) {
        Some(ValueOrContainer::Value(value)) => Ok(value),
        _ => Err(ScheduleDocumentError::Malformed(key)),
    }
}

fn encode<T: Serialize>(value: &T, part: &'static str) -> Result<LoroValue> {
    serde_json::to_value(value)
        .map(LoroValue::from)
        .map_err(|_| ScheduleDocumentError::Malformed(part))
}

fn decode<T: DeserializeOwned>(value: LoroValue, part: &'static str) -> Result<T> {
    serde_json::from_value(value.into()).map_err(|_| ScheduleDocumentError::Malformed(part))
}

fn decode_list<T: DeserializeOwned>(list: LoroList, part: &'static str) -> Result<Vec<T>> {
    list.to_vec()
        .into_iter()
        .map(|value| decode(value, part))
        .collect()
}

fn routine_identity(
    definition: &ScheduleDefinition,
    workspace: WorkspaceId,
    epoch: Option<HandoffId>,
) -> Result<RoutineDefinitionIdentity> {
    Ok(RoutineDefinitionIdentity {
        revision: definition.revision,
        digest: koru_delegate_schedule::routine_definition_digest(definition, workspace, epoch)
            .map_err(|_| ScheduleDocumentError::ClaimBindingMismatch)?,
        owner_handoff: epoch,
    })
}

fn bind_routine(
    header: &ScheduleHeader,
    workspace: WorkspaceId,
    device: DeviceId,
    epoch: Option<HandoffId>,
    prepared: RoutinePrepared,
    representative_miss: Option<RoutineRepresentativeMiss>,
) -> Result<BoundScheduleClaim> {
    let definition = &header.definition;
    let association = definition
        .routine
        .as_ref()
        .ok_or(ScheduleDocumentError::ClaimBindingMismatch)?;
    let submission = &prepared.submission;
    let slot = RoutineSlot {
        owner: association.owner,
        bot: association.bot,
        schedule: header.schedule,
        occurrence_ms: prepared.slot.occurrence_ms,
    };
    // The prepared union may also contain Role and Knowledge documents. Explicit
    // task pointers must each be present; inert template execution settings are
    // deliberately not copied over the independently captured Bot submission.
    if !prepared.valid()
        || prepared.slot != slot
        || association.home != workspace
        || association.owner.device != device
        || submission.workspace != workspace
        || prepared.definition != routine_identity(definition, workspace, epoch)?
        || submission.request.objective != definition.template.objective
        || koru_delegate_core::Objective::new(&submission.input.text).ok().as_ref()
            != Some(&definition.template.objective)
        || !submission.input.attachment_ids.is_empty()
        || !submission.input.references.is_empty()
        || !submission.input.skills.is_empty()
        || submission.request.context.len() > 33
        || definition.template.context.len() > 16
        || !definition.template.context.iter().all(|pointer| {
            matches!(pointer, koru_delegate_schedule::ScheduleContextPointer::Document { workspace: home, .. } if *home == workspace)
                && submission.request.context.iter().any(|pin|
                    context_matches(std::slice::from_ref(pointer), std::slice::from_ref(pin)))
        })
        || prepared.slot.occurrence_ms < definition.effective_at_ms
        || representative_miss.is_some_and(|miss| {
            miss.occurrence_ms < definition.effective_at_ms
                || miss.occurrence_ms >= prepared.slot.occurrence_ms
                || miss.planned_run == prepared.ids.run
                || miss.planned_run.to_storage().parse::<RunId>().ok() != Some(miss.planned_run)
        })
    {
        return Err(ScheduleDocumentError::ClaimBindingMismatch);
    }
    Ok(BoundScheduleClaim::BotRoutine {
        prepared,
        representative_miss,
    })
}

fn bind_plan(
    template: &DelegationTemplate,
    owner: DeviceId,
    plan: ScheduleClaimPlan,
) -> Result<BoundScheduleClaim> {
    match plan {
        ScheduleClaimPlan::Run {
            occurrence_ms,
            run,
            pinned_context,
        } if context_matches(&template.context, &pinned_context) => Ok(BoundScheduleClaim::Run {
            occurrence_ms,
            request: template.clone().into_request(run, owner, pinned_context),
        }),
        ScheduleClaimPlan::Skip {
            occurrence_ms,
            run,
            outcome,
            busy_holder,
        } if outcome != ScheduleFiringOutcome::Fired
            && (outcome == ScheduleFiringOutcome::SkippedBusy) == busy_holder.is_some() =>
        {
            Ok(BoundScheduleClaim::Skip {
                occurrence_ms,
                run,
                outcome,
                busy_holder,
            })
        }
        _ => Err(ScheduleDocumentError::ClaimBindingMismatch),
    }
}

fn preserve_routine(current: &ScheduleDefinition, offered: &ScheduleDefinition) -> Result<()> {
    if current.routine != offered.routine {
        return Err(ScheduleDocumentError::RoutineAssociationChanged);
    }
    if current.routine.is_some() {
        let (before, after) = (&current.template, &offered.template);
        if before.requested_by != after.requested_by
            || before.agent != after.agent
            || before.requested_model != after.requested_model
            || before.scope != after.scope
            || before.grants != after.grants
        {
            return Err(ScheduleDocumentError::RoutineTemplateChanged);
        }
    }
    Ok(())
}

fn context_matches(
    pointers: &[koru_delegate_schedule::ScheduleContextPointer],
    pins: &[RunContextItem],
) -> bool {
    if pointers.len() != pins.len() {
        return false;
    }
    pointers
        .iter()
        .zip(pins)
        .all(|(pointer, pin)| match (pointer, pin) {
            (
                koru_delegate_schedule::ScheduleContextPointer::Document {
                    workspace: pointer_workspace,
                    document: pointer_document,
                },
                RunContextItem::Document {
                    workspace: pin_workspace,
                    document: pin_document,
                    ..
                },
            ) => pointer_workspace == pin_workspace && pointer_document == pin_document,
            (
                koru_delegate_schedule::ScheduleContextPointer::Block {
                    workspace: pointer_workspace,
                    document: pointer_document,
                    node: pointer_node,
                },
                RunContextItem::Block {
                    workspace: pin_workspace,
                    document: pin_document,
                    node: pin_node,
                    ..
                },
            ) => {
                pointer_workspace == pin_workspace
                    && pointer_document == pin_document
                    && pointer_node == pin_node
            }
            (
                koru_delegate_schedule::ScheduleContextPointer::Blob {
                    workspace: pointer_workspace,
                    blob: pointer_blob,
                    blob_kind: pointer_kind,
                },
                RunContextItem::Blob {
                    workspace: pin_workspace,
                    blob: pin_blob,
                    blob_kind: pin_kind,
                },
            ) => {
                pointer_workspace == pin_workspace
                    && pointer_blob == pin_blob
                    && pointer_kind == pin_kind
            }
            _ => false,
        })
}

fn append_handoff_once(list: &LoroList, event: &HandoffEvent) -> Result<ScheduleWrite> {
    let same_kind = |known: &HandoffEvent| {
        matches!((known, event),
        (HandoffEvent::Requested { handoff: a, .. }, HandoffEvent::Requested { handoff: b, .. }) |
        (HandoffEvent::Confirmed { handoff: a, .. }, HandoffEvent::Confirmed { handoff: b, .. }) if a == b)
    };
    if let Some(known) = decode_list::<HandoffEvent>(list.clone(), HANDOFFS)?
        .into_iter()
        .find(same_kind)
    {
        return if known == *event {
            Ok(ScheduleWrite::AlreadyApplied)
        } else {
            Err(ScheduleDocumentError::HandoffConflict)
        };
    }
    list.push(encode(event, HANDOFFS)?)?;
    Ok(ScheduleWrite::Applied)
}
