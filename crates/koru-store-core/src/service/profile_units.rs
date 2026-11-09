// SPDX-License-Identifier: MPL-2.0

//! Independently durable Profile content, addressed by owner and unit.
//!
//! Personal data is **v2** here (`docs/features/root-catalog/TECHNICAL.final.md`
//! §5.1): one unit per Workspace under the device's user scope, read and
//! written through the `workspace_personal_*` ports. The facade stays
//! Profile-oriented for its callers — `reminders(profile)` and friends answer
//! over the Workspaces whose **current placement** is that Profile, read from
//! the root Workspace unit (`root_move.rs`) — but no personal row is keyed by
//! a Profile any more, and the former Profile of a Workspace need not exist
//! for its personal data to be found.

use super::*;
use crate::ids::UserScopeId;
use crate::personal_attributes::{PersonalAttributeValue, PersonalAttributes};
use crate::ports::{
    ProfileUnitState, ReminderDue, WorkspacePersonalUnitState, WorkspaceReminderDue,
};
use crate::profile_units::{
    IdentityManifest, LegacyPersonalSource, PersonalManifest, PrivateManifest, ProfileUnitRef,
    admit_legacy_source,
};

impl<C: Catalog, B: Backend> Store<C, B> {
    pub(super) fn require_profile(&self, profile: ProfileId) -> Result<()> {
        match self.catalog.profile_catalog_state(profile)? {
            Some(ProfileCatalogState::Active) => Ok(()),
            Some(ProfileCatalogState::Removed) => Err(Error::bare(Code::ProfileRemoved)),
            None => Err(Error::bare(Code::ProfileNotFound)),
        }
    }

    fn load_private(&mut self, profile: ProfileId) -> Result<&PrivateManifest> {
        self.require_profile(profile)?;
        if !self.private_profiles.contains_key(&profile) {
            let state = self
                .catalog
                .profile_unit(ProfileUnitRef::Private { profile })?
                .ok_or_else(|| Error::bare(Code::ProfilePrivateUnavailable))?;
            let doc = PrivateManifest::from_snapshot(&state.snapshot)?;
            if doc.profile_id() != profile {
                return Err(Error::bare(Code::ProfileUnitScopeMismatch));
            }
            doc.set_peer_id(self.peer_id)?;
            self.private_profiles.insert(profile, doc);
        }
        Ok(&self.private_profiles[&profile])
    }

    /// The device-local user scope every personal v2 row is keyed by.
    fn user_scope(&self) -> Result<UserScopeId> {
        self.catalog.local_user_scope()
    }

    // Which Workspaces are placed under a Profile — `placed_workspaces` — is
    // the root catalog's answer and lives in `root_move.rs`: the Workspace
    // unit's `placement.current` (overlay first), with the `workspaces.owner`
    // projection only for a Workspace that has no unit yet. Nothing else in
    // the facade knows where the answer came from.

    /// Open a committed v2 row under its persisted writer. A read-only unit
    /// refuses the writer, as every unit does, and the caller reports it.
    pub(super) fn open_personal(
        workspace: WorkspaceId,
        state: &WorkspacePersonalUnitState,
    ) -> Result<PersonalManifest> {
        let doc = PersonalManifest::from_snapshot_v2(&state.snapshot, workspace)?;
        doc.set_peer_id(state.writer_peer_id)?;
        Ok(doc)
    }

    /// The v2 unit of `workspace`, created on first use with a fresh writer
    /// that the first commit persists. `owner` is the Profile the caller
    /// acts as; it must be active, but it is not part of the unit's address
    /// and the Workspace's former Profile need not exist at all.
    fn personal_candidate(
        &self,
        owner: ProfileId,
        workspace: WorkspaceId,
    ) -> Result<PersonalManifest> {
        self.require_profile(owner)?;
        if self.catalog.workspace(workspace)?.is_none() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        // **The Profile in the call still has to hold the Workspace.** The
        // unit itself is addressed by `(user scope, workspace)` so that
        // personal data follows a move (§5.1), and every Profile on one
        // device shares that scope — so without this check any active
        // Profile could read and overwrite the personal state of a Workspace
        // placed under another one. §5.1 is explicit that the
        // Profile-oriented facade "resolves the user's workspace personal
        // unit using current placement"; the list-shaped reads
        // (`personal_documents`, `reminders`) always did, and these addressed
        // ones did not until a `personal_attributes` test caught it.
        if !self.placed_workspaces(owner)?.contains(&workspace) {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        match self
            .catalog
            .workspace_personal_unit(self.user_scope()?, workspace)?
        {
            Some(state) => Self::open_personal(workspace, &state),
            None => PersonalManifest::create_v2(workspace, mint_peer_id()),
        }
    }

    /// Every personal unit of a Workspace currently placed under `owner`.
    fn personal_documents(&self, owner: ProfileId) -> Result<Vec<PersonalManifest>> {
        self.require_profile(owner)?;
        let placed = self.placed_workspaces(owner)?;
        self.catalog
            .workspace_personal_units(self.user_scope()?)?
            .into_iter()
            .filter(|(workspace, _)| placed.contains(workspace))
            .map(|(workspace, state)| Self::open_personal(workspace, &state))
            .collect()
    }

    fn save_personal(&mut self, doc: &PersonalManifest) -> Result<()> {
        let scope = self.user_scope()?;
        self.catalog.commit_workspace_personal_unit(
            scope,
            doc.workspace(),
            &doc.export_snapshot()?,
            doc.writer_peer(),
        )
    }

    fn locate_reminder(
        &self,
        owner: ProfileId,
        reminder: ReminderId,
    ) -> Result<(PersonalManifest, StoredReminder)> {
        let mut found = None;
        for doc in self.personal_documents(owner)? {
            if let Some(row) = doc.reminder(reminder) {
                if found.is_some() {
                    return Err(Error::bare(Code::ProfileUnitRequired));
                }
                found = Some((doc, row));
            }
        }
        found.ok_or_else(|| Error::bare(Code::ReminderNotFound))
    }

    pub fn personal_attributes(
        &mut self,
        profile: ProfileId,
        entity: &EntityRef,
    ) -> Result<PersonalAttributes> {
        self.require_profile(profile)?;
        // Placement decides, as it does for `personal_candidate` and for
        // every list-shaped read: a Profile that does not hold this Workspace
        // has no personal state for it. An empty answer rather than a
        // refusal, because that is what "this Profile has nothing here"
        // means to a reader, and it is what the route answered when the unit
        // was addressed by Profile.
        if !self
            .placed_workspaces(profile)?
            .contains(&entity.workspace())
        {
            return Ok(PersonalAttributes::new());
        }
        let Some(state) = self
            .catalog
            .workspace_personal_unit(self.user_scope()?, entity.workspace())?
        else {
            return Ok(PersonalAttributes::new());
        };
        let doc = PersonalManifest::from_snapshot_v2(&state.snapshot, entity.workspace())?;
        if doc.is_read_only() {
            return Err(Error::bare(Code::SchemaTooNew));
        }
        Ok(doc.personal_attributes(entity))
    }

    pub fn set_personal_attribute(
        &mut self,
        profile: ProfileId,
        entity: &EntityRef,
        key: &str,
        value: Option<&PersonalAttributeValue>,
    ) -> Result<PersonalAttributes> {
        let doc = self.personal_candidate(profile, entity.workspace())?;
        let changed = match value {
            Some(value) => {
                doc.set_personal_attribute(entity, key, value)?;
                true
            }
            None => doc.clear_personal_attribute(entity, key)?,
        };
        if changed {
            self.save_personal(&doc)?;
        }
        Ok(doc.personal_attributes(entity))
    }

    fn locate_reminder_in(
        &self,
        owner: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
    ) -> Result<(PersonalManifest, StoredReminder)> {
        let doc = self.personal_candidate(owner, workspace)?;
        if doc.is_read_only() {
            return Err(Error::bare(Code::SchemaTooNew));
        }
        let row = doc
            .reminder(reminder)
            .ok_or_else(|| Error::bare(Code::ReminderNotFound))?;
        Ok((doc, row))
    }

    pub fn reminder_in(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
    ) -> Result<StoredReminder> {
        self.locate_reminder_in(profile, workspace, reminder)
            .map(|(_, row)| row)
    }

    pub fn reminder(&mut self, profile: ProfileId, reminder: ReminderId) -> Result<StoredReminder> {
        self.locate_reminder(profile, reminder).map(|(_, row)| row)
    }

    pub fn reminders(&mut self, profile: ProfileId) -> Result<Vec<StoredReminder>> {
        Ok(self
            .personal_documents(profile)?
            .into_iter()
            .flat_map(|doc| doc.reminders())
            .collect())
    }

    /// A complete index is required before native delivery can interpret an
    /// empty result. The asynchronous adapter repairs stale units off-thread.
    pub fn reminder_reduction_inputs(
        &mut self,
        profile: ProfileId,
    ) -> Result<Vec<ReminderReductionInput>> {
        self.require_profile(profile)?;
        let placed = self.placed_workspaces(profile)?;
        let scope = self.user_scope()?;
        let mut out = Vec::new();
        let indexed_rows = self.catalog.workspace_reminder_due(scope)?;
        for (workspace, state) in self.catalog.workspace_personal_units(scope)? {
            if !placed.contains(&workspace) {
                continue;
            }
            let doc = PersonalManifest::from_snapshot_v2(&state.snapshot, workspace)?;
            if doc.is_read_only() {
                return Err(Error::bare(Code::SchemaTooNew));
            }
            let indexed = state
                .indexed_version
                .as_deref()
                .and_then(|v| loro::VersionVector::decode(v).ok());
            if indexed.as_ref() != Some(&doc.version()) {
                return Err(Error::bare(Code::ReminderIndexUpdating));
            }
            let inputs = doc.reminder_reduction_inputs();
            let ids: BTreeSet<_> = indexed_rows
                .iter()
                .filter(|row| row.workspace == workspace)
                .map(|row| row.reminder)
                .collect();
            let actual: BTreeSet<_> = inputs.iter().map(|row| row.stored().rule.id).collect();
            if ids != actual {
                return Err(Error::bare(Code::ReminderIndexUpdating));
            }
            out.extend(inputs);
        }
        let mut seen = BTreeSet::new();
        if out.iter().any(|row| !seen.insert(row.stored().rule.id)) {
            return Err(Error::bare(Code::ProfileUnitRequired));
        }
        Ok(out)
    }

    /// Opaque candidates are taken under the Store lock; Loro decoding and
    /// reducer work run on the adapter's existing blocking worker.
    ///
    /// Each candidate is a v2 row presented in the facade's Profile-oriented
    /// shape: `unit` is `Personal { owner: profile, workspace }` where
    /// `owner` is the Profile the Workspace is **currently placed** under,
    /// not an address the row is stored by. [`build_reminder_index`] and
    /// [`read_personal_reminder_inputs`] decode the bytes as v2, and the
    /// `owner` travels back through [`Self::publish_reminder_index`] only to
    /// be checked against the rows.
    pub fn reminder_index_candidates(&self, profile: ProfileId) -> Result<Vec<ProfileUnitState>> {
        self.require_profile(profile)?;
        let placed = self.placed_workspaces(profile)?;
        Ok(self
            .catalog
            .workspace_personal_units(self.user_scope()?)?
            .into_iter()
            .filter(|(workspace, _)| placed.contains(workspace))
            .map(|(workspace, state)| ProfileUnitState {
                unit: ProfileUnitRef::Personal {
                    owner: profile,
                    workspace,
                },
                snapshot: state.snapshot,
                indexed_version: state.indexed_version,
            })
            .collect())
    }

    pub fn reminder_index_rows(&self, profile: ProfileId) -> Result<Vec<ReminderDue>> {
        self.require_profile(profile)?;
        let placed = self.placed_workspaces(profile)?;
        Ok(self
            .catalog
            .workspace_reminder_due(self.user_scope()?)?
            .into_iter()
            .filter(|row| placed.contains(&row.workspace))
            .map(|row| ReminderDue {
                owner: profile,
                workspace: row.workspace,
                reminder: row.reminder,
                due_at_ms: row.due_at_ms,
            })
            .collect())
    }

    pub fn publish_reminder_index(
        &mut self,
        source: &ProfileUnitState,
        version: &[u8],
        rows: &[ReminderDue],
    ) -> Result<bool> {
        let ProfileUnitRef::Personal { owner, workspace } = source.unit else {
            return Err(Error::bare(Code::ProfileUnitScopeMismatch));
        };
        self.require_profile(owner)?;
        if rows
            .iter()
            .any(|row| row.owner != owner || row.workspace != workspace)
        {
            return Err(Error::new(
                Code::InvalidRequest,
                "reminder index scope mismatch",
            ));
        }
        let rows: Vec<WorkspaceReminderDue> = rows
            .iter()
            .map(|row| WorkspaceReminderDue {
                workspace: row.workspace,
                reminder: row.reminder,
                due_at_ms: row.due_at_ms,
            })
            .collect();
        let scope = self.user_scope()?;
        self.catalog
            .replace_workspace_due_index(scope, workspace, &source.snapshot, version, &rows)
    }

    pub fn reminders_for(
        &mut self,
        profile: ProfileId,
        target: &EntityRef,
    ) -> Result<Vec<StoredReminder>> {
        Ok(self
            .reminders(profile)?
            .into_iter()
            .filter(|row| row.rule.target == *target)
            .collect())
    }

    pub fn create_reminder(
        &mut self,
        profile: ProfileId,
        rule: &ReminderRule,
    ) -> Result<StoredReminder> {
        if rule.status == RuleStatus::Deleted {
            return Err(Error::new(
                Code::InvalidRequest,
                "deleted status requires the monotonic delete operation",
            ));
        }
        let doc = self.personal_candidate(profile, rule.target.workspace())?;
        for other in self.personal_documents(profile)? {
            if other.workspace() != doc.workspace() && other.reminder(rule.id).is_some() {
                return Err(Error::bare(Code::ProfileUnitRequired));
            }
        }
        if let Some(existing) = doc.reminder(rule.id) {
            return create_retry(&existing, rule).map(|_| existing);
        }
        doc.put_reminder_rule(rule)?;
        self.save_personal(&doc)?;
        self.record_reminder_change(profile, Some(rule.id), ReminderChange::Rule);
        doc.reminder(rule.id)
            .ok_or_else(|| Error::bare(Code::ReminderNotFound))
    }

    pub fn update_reminder(
        &mut self,
        profile: ProfileId,
        rule: &ReminderRule,
    ) -> Result<StoredReminder> {
        let (doc, _) = self.locate_reminder(profile, rule.id)?;
        self.update_reminder_in(profile, doc.workspace(), rule)
    }

    pub fn update_reminder_in(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        rule: &ReminderRule,
    ) -> Result<StoredReminder> {
        if rule.status == RuleStatus::Deleted {
            return Err(Error::new(
                Code::InvalidRequest,
                "deleted status requires the monotonic delete operation",
            ));
        }
        let (doc, existing) = self.locate_reminder_in(profile, workspace, rule.id)?;
        if doc.workspace() != rule.target.workspace() {
            return Err(Error::bare(Code::ProfileUnitScopeMismatch));
        }
        if !update_writes(&existing, rule)? {
            return Ok(existing);
        }
        doc.put_reminder_rule(rule)?;
        self.save_personal(&doc)?;
        self.record_reminder_change(profile, Some(rule.id), ReminderChange::Rule);
        doc.reminder(rule.id)
            .ok_or_else(|| Error::bare(Code::ReminderNotFound))
    }

    pub fn set_reminder_status_in(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
        revision: RuleRevisionId,
        status: RuleStatus,
    ) -> Result<StoredReminder> {
        let mut rule = self.reminder_in(profile, workspace, reminder)?.rule;
        rule.revision = revision;
        rule.status = status;
        self.update_reminder_in(profile, workspace, &rule)
    }

    pub fn set_reminder_status(
        &mut self,
        profile: ProfileId,
        reminder: ReminderId,
        revision: RuleRevisionId,
        status: RuleStatus,
    ) -> Result<StoredReminder> {
        let mut rule = self.reminder(profile, reminder)?.rule;
        rule.revision = revision;
        rule.status = status;
        self.update_reminder(profile, &rule)
    }

    pub fn delete_reminder_rule(
        &mut self,
        profile: ProfileId,
        reminder: ReminderId,
    ) -> Result<StoredReminder> {
        let (doc, _) = self.locate_reminder(profile, reminder)?;
        self.delete_reminder_rule_in(profile, doc.workspace(), reminder)
    }

    pub fn delete_reminder_rule_in(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
    ) -> Result<StoredReminder> {
        let (doc, existing) = self.locate_reminder_in(profile, workspace, reminder)?;
        if existing.deleted {
            return Ok(existing);
        }
        doc.delete_reminder(reminder)?;
        self.save_personal(&doc)?;
        self.record_reminder_change(profile, Some(reminder), ReminderChange::Rule);
        doc.reminder(reminder)
            .ok_or_else(|| Error::bare(Code::ReminderNotFound))
    }

    pub fn delete_reminders_for_target(
        &mut self,
        profile: ProfileId,
        target: &EntityRef,
    ) -> Result<Vec<ReminderId>> {
        let doc = self.personal_candidate(profile, target.workspace())?;
        let removed = doc.delete_reminders_for_target(target)?;
        if !removed.is_empty() {
            self.save_personal(&doc)?;
            for id in &removed {
                self.record_reminder_change(profile, Some(*id), ReminderChange::Rule);
            }
        }
        Ok(removed)
    }

    pub fn append_reminder_action(
        &mut self,
        profile: ProfileId,
        reminder: ReminderId,
        action: &OccurrenceAction,
    ) -> Result<StoredReminder> {
        let (doc, _) = self.locate_reminder(profile, reminder)?;
        self.append_reminder_action_in(profile, doc.workspace(), reminder, action)
    }

    pub fn append_reminder_action_in(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
        action: &OccurrenceAction,
    ) -> Result<StoredReminder> {
        let (doc, existing) = self.locate_reminder_in(profile, workspace, reminder)?;
        if existing.deleted {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "a deleted reminder cannot accept actions",
            ));
        }
        if let Some(prior) = doc.raw_reminder_action(reminder, action.id) {
            return action_retry(&prior, action).map(|_| existing);
        }
        doc.append_reminder_action(reminder, action)?;
        self.save_personal(&doc)?;
        self.record_reminder_change(profile, Some(reminder), ReminderChange::Action);
        doc.reminder(reminder)
            .ok_or_else(|| Error::bare(Code::ReminderNotFound))
    }

    pub fn clear_reminder_history(&mut self, profile: ProfileId) -> Result<usize> {
        let mut removed = 0;
        // One commit per Workspace unit. The v1 facade batched every unit of
        // a Profile into one catalog transaction; the v2 port commits a unit
        // at a time, and the clear facts are grow-only and idempotent, so a
        // commit that fails midway leaves earlier units cleared and the rest
        // exactly as they were, ready for the retry.
        for doc in self.personal_documents(profile)? {
            let count = doc.clear_handled_reminder_history()?;
            if count > 0 {
                removed += count;
                self.save_personal(&doc)?;
            }
        }
        if removed > 0 {
            self.record_reminder_change(profile, None, ReminderChange::History);
        }
        Ok(removed)
    }

    pub fn clear_reminder_history_for_target(
        &mut self,
        profile: ProfileId,
        target: &EntityRef,
    ) -> Result<usize> {
        let doc = self.personal_candidate(profile, target.workspace())?;
        let removed = doc.clear_handled_reminder_history_for_target(target)?;
        if removed > 0 {
            self.save_personal(&doc)?;
            self.record_reminder_change(profile, None, ReminderChange::History);
        }
        Ok(removed)
    }

    pub fn reminder_settings(&mut self, profile: ProfileId) -> Result<ReminderSettings> {
        let doc = self.load_private(profile)?;
        if doc.is_read_only() {
            return Err(Error::bare(Code::SchemaTooNew));
        }
        Ok(doc.reminder_settings())
    }

    pub fn update_reminder_settings(
        &mut self,
        profile: ProfileId,
        end_of_day: Option<Option<crate::reminders::LocalTime>>,
        default_tone: Option<crate::reminders::ToneId>,
        week_start: Option<crate::reminders::WeekStartPreference>,
    ) -> Result<ReminderSettings> {
        let snapshot = self.load_private(profile)?.export_snapshot()?;
        let doc = PrivateManifest::from_snapshot(&snapshot)?;
        doc.set_peer_id(self.peer_id)?;
        if doc.update_reminder_settings(end_of_day, default_tone, week_start)? {
            self.catalog.commit_profile_unit(
                ProfileUnitRef::Private { profile },
                &doc.export_snapshot()?,
            )?;
            self.record_reminder_change(profile, None, ReminderChange::Settings);
        }
        let result = doc.reminder_settings();
        self.private_profiles.insert(profile, doc);
        Ok(result)
    }

    pub fn personal_dictionary(
        &mut self,
        profile: ProfileId,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        Ok(self.load_private(profile)?.dictionary())
    }

    pub fn set_dictionary_word(
        &mut self,
        profile: ProfileId,
        language: &str,
        word: &str,
        present: bool,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let snapshot = self.load_private(profile)?.export_snapshot()?;
        let doc = PrivateManifest::from_snapshot(&snapshot)?;
        doc.set_peer_id(self.peer_id)?;
        let changed = if present {
            doc.add_dictionary_word(language, word)?;
            true
        } else {
            doc.remove_dictionary_word(language, word)?
        };
        if changed {
            self.catalog.commit_profile_unit(
                ProfileUnitRef::Private { profile },
                &doc.export_snapshot()?,
            )?;
            self.record_dictionary_change(profile);
        }
        let result = doc.dictionary();
        self.private_profiles.insert(profile, doc);
        Ok(result)
    }

    pub fn clear_personal_dictionary(
        &mut self,
        profile: ProfileId,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let snapshot = self.load_private(profile)?.export_snapshot()?;
        let doc = PrivateManifest::from_snapshot(&snapshot)?;
        doc.set_peer_id(self.peer_id)?;
        if doc.clear_dictionary()? > 0 {
            self.catalog.commit_profile_unit(
                ProfileUnitRef::Private { profile },
                &doc.export_snapshot()?,
            )?;
            self.record_dictionary_change(profile);
        }
        let result = doc.dictionary();
        self.private_profiles.insert(profile, doc);
        Ok(result)
    }

    pub(super) fn record_reminder_change(
        &mut self,
        profile: ProfileId,
        reminder: Option<ReminderId>,
        change: ReminderChange,
    ) {
        self.reminder_committed.insert(
            (profile, reminder, change),
            ReminderCommitted {
                profile,
                reminder,
                change,
            },
        );
    }

    fn record_dictionary_change(&mut self, profile: ProfileId) {
        self.dictionary_committed
            .insert(profile, DictionaryCommitted { profile });
    }

    fn record_identity_change(&mut self, profile: ProfileId, change: IdentityChange) {
        self.identity_committed
            .insert((profile, change), IdentityCommitted { profile, change });
    }

    pub fn profile_unit_states(&self, profile: ProfileId) -> Result<Vec<ProfileUnitState>> {
        self.require_profile(profile)?;
        self.catalog.profile_units(profile)
    }

    pub fn profile_unit_version(&mut self, unit: ProfileUnitRef) -> Result<loro::VersionVector> {
        self.require_profile(unit.profile())?;
        personal_v1_exchange_fenced(unit)?;
        let Some(state) = self.catalog.profile_unit(unit)? else {
            return Ok(loro::VersionVector::new());
        };
        unit_version(unit, &state.snapshot)
    }

    /// The exact committed snapshot of one unit, never a synthesized empty unit
    /// or an enumeration/export of the other units belonging to its Profile.
    pub fn profile_unit_snapshot(&self, unit: ProfileUnitRef) -> Result<Vec<u8>> {
        self.require_profile(unit.profile())?;
        personal_v1_exchange_fenced(unit)?;
        let state = self
            .catalog
            .profile_unit(unit)?
            .ok_or_else(|| Error::bare(Code::VersionUnavailable))?;
        unit_version(unit, &state.snapshot)?;
        Ok(state.snapshot)
    }

    pub fn profile_unit_updates_since(
        &mut self,
        unit: ProfileUnitRef,
        from: &loro::VersionVector,
    ) -> Result<Vec<u8>> {
        self.require_profile(unit.profile())?;
        personal_v1_exchange_fenced(unit)?;
        let state = self
            .catalog
            .profile_unit(unit)?
            .ok_or_else(|| Error::bare(Code::VersionUnavailable))?;
        unit_version(unit, &state.snapshot)?;
        match unit {
            ProfileUnitRef::LegacyPrivate { .. } => {
                ProfileManifest::from_snapshot(&state.snapshot)?.export_updates_since(from)
            }
            ProfileUnitRef::Identity { .. } => {
                IdentityManifest::from_snapshot(&state.snapshot)?.export_updates_since(from)
            }
            ProfileUnitRef::Private { .. } => {
                PrivateManifest::from_snapshot(&state.snapshot)?.export_updates_since(from)
            }
            ProfileUnitRef::Personal { .. } => Err(personal_v1_exchange_fenced_error()),
        }
    }

    /// Compatibility for local callers that explicitly mean identity. Peer
    /// frames use apply_profile_unit_updates and cannot omit the discriminator.
    pub fn apply_profile_updates(&mut self, profile: ProfileId, updates: &[Vec<u8>]) -> Result<()> {
        self.apply_profile_unit_updates(ProfileUnitRef::Identity { profile }, updates)
    }

    pub fn apply_profile_unit_updates(
        &mut self,
        unit: ProfileUnitRef,
        updates: &[Vec<u8>],
    ) -> Result<()> {
        let profile = unit.profile();
        if self.catalog.profile_catalog_state(profile)? == Some(ProfileCatalogState::Removed) {
            return Err(Error::bare(Code::ProfileRemoved));
        }
        let before = self.catalog.profile_unit(unit)?;
        match unit {
            ProfileUnitRef::Identity { .. } => {
                // Parsed once and kept, because what it held before the import
                // is the only way to say *which* identity fact moved. The
                // Private and Personal branches below read their priors for the
                // same reason.
                let prior = before
                    .as_ref()
                    .map(|state| IdentityManifest::from_snapshot(&state.snapshot))
                    .transpose()?;
                let prior_identity = prior.as_ref().map(IdentityManifest::profile).transpose()?;
                let doc = match &prior {
                    Some(doc) => doc.staged_import(updates)?,
                    None => IdentityManifest::from_snapshot(&complete_unit_snapshot(updates)?)?,
                };
                if doc.unit() != unit {
                    return Err(Error::bare(Code::ProfileUnitScopeMismatch));
                }
                if doc.is_read_only() {
                    return Err(Error::bare(Code::SchemaTooNew));
                }
                let identity = doc.profile()?;
                doc.set_peer_id(self.peer_id)?;
                self.catalog
                    .commit_profile_identity(&identity, &doc.export_snapshot()?)?;
                self.open_profiles.insert(profile, doc);
                // Only against a unit this Store already held. A first import
                // is a profile arriving, not a profile changing, and saying
                // "renamed" about something nobody has seen yet would be a
                // lie; that case belongs to whatever announces a new profile.
                if let Some(prior_identity) = prior_identity {
                    if prior_identity.name != identity.name {
                        self.record_identity_change(profile, IdentityChange::Name);
                    }
                    if prior_identity.icon != identity.icon
                        || prior_identity.color != identity.color
                    {
                        self.record_identity_change(profile, IdentityChange::Appearance);
                    }
                }
            }
            ProfileUnitRef::Private { .. } => {
                self.require_profile(profile)?;
                let prior = before
                    .as_ref()
                    .map(|s| PrivateManifest::from_snapshot(&s.snapshot))
                    .transpose()?;
                let prior_settings = prior.as_ref().map(PrivateManifest::reminder_settings);
                let prior_dictionary = prior.as_ref().map(PrivateManifest::dictionary);
                let doc = match &before {
                    Some(state) => {
                        PrivateManifest::from_snapshot(&state.snapshot)?.staged_import(updates)?
                    }
                    None => PrivateManifest::from_snapshot(&complete_unit_snapshot(updates)?)?,
                };
                if doc.unit() != unit {
                    return Err(Error::bare(Code::ProfileUnitScopeMismatch));
                }
                if doc.is_read_only() {
                    return Err(Error::bare(Code::SchemaTooNew));
                }
                doc.set_peer_id(self.peer_id)?;
                let changed = prior_settings.as_ref() != Some(&doc.reminder_settings());
                self.catalog
                    .commit_profile_unit(unit, &doc.export_snapshot()?)?;
                self.private_profiles.insert(profile, doc);
                if changed {
                    self.record_reminder_change(profile, None, ReminderChange::Settings);
                }
                if prior_dictionary.unwrap_or_default()
                    != self.private_profiles[&profile].dictionary()
                {
                    self.record_dictionary_change(profile);
                }
            }
            ProfileUnitRef::Personal { owner, workspace } => {
                // A v1 personal frame on the v1 address, after this device
                // moved to v2. §5.2 step 6: a complete snapshot goes through
                // the same normalization the migration used and is staged
                // into the Workspace's v2 unit; a delta is fenced, because
                // its ancestors live in a namespace this unit no longer
                // holds under their original ids. There is no second live
                // v1 path.
                self.require_profile(profile)?;
                if self.catalog.workspace(workspace)?.is_none() {
                    return Err(Error::bare(Code::WorkspaceNotFound));
                }
                let scope = self.user_scope()?;
                let held = match self.catalog.workspace_personal_unit(scope, workspace)? {
                    Some(state) => Some(Self::open_personal(workspace, &state)?),
                    None => None,
                };
                let old_rows = held
                    .as_ref()
                    .map(|d| d.reminders())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| (r.rule.id, r))
                    .collect();
                let old_actions = held
                    .as_ref()
                    .map(|d| d.raw_reminder_actions())
                    .unwrap_or_default();
                let old_cleared = held
                    .as_ref()
                    .map(|d| d.cleared_reminder_actions())
                    .unwrap_or_default();
                let mut doc = match held {
                    Some(doc) => doc,
                    None => PersonalManifest::create_v2(workspace, mint_peer_id())?,
                };
                for frame in updates {
                    let probe = loro::LoroDoc::new();
                    let status = probe
                        .import(frame)
                        .map_err(|_| Error::bare(Code::CrdtRejected))?;
                    if status.pending.is_some() {
                        return Err(Error::new(
                            Code::CrdtRejected,
                            "v1 personal deltas are fenced; personal units are v2 and admit only a complete v1 snapshot",
                        ));
                    }
                    let source = LegacyPersonalSource {
                        owner,
                        workspace,
                        snapshot: frame.clone(),
                    };
                    let (merged, receipt) = admit_legacy_source(&doc, &source, mint_peer_id())?;
                    tracing::info!(
                        changes = receipt.change_count,
                        ops = receipt.op_count,
                        peers = receipt.peers.len(),
                        "admitted a legacy personal snapshot into the workspace's v2 unit",
                    );
                    doc = merged;
                }
                if doc.is_read_only() {
                    return Err(Error::bare(Code::SchemaTooNew));
                }
                let new_rows = doc
                    .reminders()
                    .into_iter()
                    .map(|r| (r.rule.id, r))
                    .collect();
                let changes = classify_reminder_changes(
                    &old_rows,
                    &new_rows,
                    &old_actions,
                    &doc.raw_reminder_actions(),
                    &old_cleared,
                    &doc.cleared_reminder_actions(),
                    &ReminderSettings::default(),
                    &ReminderSettings::default(),
                );
                self.catalog.commit_workspace_personal_unit(
                    scope,
                    workspace,
                    &doc.export_snapshot()?,
                    doc.writer_peer(),
                )?;
                for (reminder, change) in changes {
                    self.record_reminder_change(profile, reminder, change);
                }
            }
            ProfileUnitRef::LegacyPrivate { .. } => {
                self.require_profile(profile)?;
                let doc = loro::LoroDoc::new();
                if let Some(before) = before {
                    doc.import(&before.snapshot)
                        .map_err(|_| Error::bare(Code::CorruptState))?;
                }
                for bytes in updates {
                    if doc
                        .import(bytes)
                        .map_err(|_| Error::bare(Code::CrdtRejected))?
                        .pending
                        .is_some()
                    {
                        return Err(Error::bare(Code::CrdtRejected));
                    }
                }
                let bytes = doc
                    .export(loro::ExportMode::snapshot())
                    .map_err(|_| Error::bare(Code::StorageFailed))?;
                if ProfileManifest::from_snapshot(&bytes)?.profile()?.id != profile {
                    return Err(Error::bare(Code::ProfileUnitScopeMismatch));
                }
                // No legacy field projects or wakes the fresh authorities.
                self.catalog.commit_profile_unit(unit, &bytes)?;
            }
        }
        Ok(())
    }
}

fn complete_unit_snapshot(updates: &[Vec<u8>]) -> Result<Vec<u8>> {
    let doc = loro::LoroDoc::new();
    for bytes in updates {
        if doc
            .import(bytes)
            .map_err(|_| Error::bare(Code::CrdtRejected))?
            .pending
            .is_some()
        {
            return Err(Error::bare(Code::CrdtRejected));
        }
    }
    doc.export(loro::ExportMode::snapshot())
        .map_err(|_| Error::bare(Code::StorageFailed))
}

fn unit_version(unit: ProfileUnitRef, snapshot: &[u8]) -> Result<loro::VersionVector> {
    match unit {
        ProfileUnitRef::Identity { .. } => {
            let doc = IdentityManifest::from_snapshot(snapshot)?;
            if doc.unit() != unit {
                return Err(Error::bare(Code::ProfileUnitScopeMismatch));
            }
            Ok(doc.version())
        }
        ProfileUnitRef::Private { .. } => {
            let doc = PrivateManifest::from_snapshot(snapshot)?;
            if doc.unit() != unit {
                return Err(Error::bare(Code::ProfileUnitScopeMismatch));
            }
            Ok(doc.version())
        }
        ProfileUnitRef::Personal { .. } => Err(personal_v1_exchange_fenced_error()),
        ProfileUnitRef::LegacyPrivate { profile } => {
            let doc = ProfileManifest::from_snapshot(snapshot)?;
            if doc.profile()?.id != profile {
                return Err(Error::bare(Code::ProfileUnitScopeMismatch));
            }
            Ok(doc.version())
        }
    }
}

/// Personal units are v2 and leave this Store only through the negotiated
/// v2 exchange of Plan 2.1 (`TECHNICAL.final.md` §11.4). The v1 address is
/// still accepted inbound for a complete snapshot (see
/// `apply_profile_unit_updates`); outbound it is refused.
fn personal_v1_exchange_fenced(unit: ProfileUnitRef) -> Result<()> {
    match unit {
        ProfileUnitRef::Personal { .. } => Err(personal_v1_exchange_fenced_error()),
        _ => Ok(()),
    }
}

fn personal_v1_exchange_fenced_error() -> Error {
    Error::new(
        Code::ProfileUnitScopeMismatch,
        "personal units are v2; the v1 personal address is not exported",
    )
}

/// The v2 unit behind a facade candidate (see
/// `Store::reminder_index_candidates` for the shape).
fn candidate_personal(source: &ProfileUnitState) -> Result<(ProfileId, PersonalManifest)> {
    let ProfileUnitRef::Personal { owner, workspace } = source.unit else {
        return Err(Error::bare(Code::ProfileUnitScopeMismatch));
    };
    let doc = PersonalManifest::from_snapshot_v2(&source.snapshot, workspace)?;
    if doc.is_read_only() {
        return Err(Error::bare(Code::SchemaTooNew));
    }
    Ok((owner, doc))
}

/// Decode a bounded personal snapshot off the Store's answering thread.
pub fn read_personal_reminder_inputs(
    source: &ProfileUnitState,
) -> Result<Vec<ReminderReductionInput>> {
    Ok(candidate_personal(source)?.1.reminder_reduction_inputs())
}

/// Decode a bounded personal snapshot off the Store's answering thread.
/// The index supplies lookup hints only; target/timezone-dependent reduction
/// remains with the existing reconciliation service.
pub fn build_reminder_index(source: &ProfileUnitState) -> Result<(Vec<u8>, Vec<ReminderDue>)> {
    let (owner, doc) = candidate_personal(source)?;
    let workspace = doc.workspace();
    let rows = doc
        .reminder_due_hints()
        .into_iter()
        .map(|(reminder, due_at_ms)| ReminderDue {
            owner,
            workspace,
            reminder,
            due_at_ms,
        })
        .collect();
    Ok((doc.version().encode(), rows))
}
