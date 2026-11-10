// SPDX-License-Identifier: MPL-2.0

//! The account path of the root catalog (root-catalog TECHNICAL.final §4.1,
//! §7.3; the contract is `docs/cloud-platform/contracts/root-catalog-cloud-v2.md`):
//! the one binding, the user-catalog admission the join runs under, and the
//! per-unit join over the account API — Profile units, then Workspace units,
//! then (after content) the account's personal units.
//!
//! Every byte that moves here is the Store's: the host's `export_root_unit`
//! is the §4.3 export boundary (prepare, derive, recheck), its
//! `join_root_unit` is the staged join with its receipt. This module only
//! decides *which* unit to ask about and carries the answer across; it never
//! reads a unit, and it logs ids and counts only.
//!
//! Nothing moves under an expired or replaced permit: every pull and every
//! push re-checks the user-catalog admission first, and a personal unit
//! moves only under its Workspace's user-personal admission.

use koru_account_core::root_catalog::CloudUnitKind;
use koru_account_wire::{
    CloudCatalogBindingRefV2, CloudRootJoinReceiptV2, CloudRootUnitUpdatesV2, Version2,
};

use super::*;

impl<A: Api, H: Host> Coordinator<A, H> {
    /// The account's catalog binding, once per generation: declare what the
    /// Store holds, record what the service answers. A 404 is a service that
    /// does not speak v2, and the pass runs the v1 pair path for this
    /// generation. A refusal of the binding is terminal for this account;
    /// the local catalog is left exactly as it was.
    pub(super) fn ensure_contract(&mut self) -> Result<(), Failure> {
        if self.contract.is_some() {
            return Ok(());
        }
        let authority = self.current_authority()?;
        let held = self.host.account_binding()?;
        let declared = held.map(declared_binding).transpose()?;
        match self.authorized(|api, authority| api.bind_catalog(authority, declared)) {
            Ok(binding) => {
                if binding.account_id != authority.owner.account {
                    return Err(Failure::DatasetMismatch);
                }
                // The Store refuses another account's catalog and a renamed
                // scope on its own; the same binding again is a no-op.
                self.host.bind_account_scope(&binding)?;
                self.catalog_scope_id = Some(binding.catalog_scope_id);
                self.contract = Some(CloudContract::V2);
                tracing::info!(
                    target: "koru_cloud",
                    catalog_scope = %binding.catalog_scope_id,
                    binding_generation = binding.binding_generation,
                    "cloud pass: account catalog bound"
                );
                Ok(())
            }
            Err(Failure::RouteNotFound) => {
                tracing::info!(
                    target: "koru_cloud",
                    "cloud pass: the service does not answer the v2 catalog; v1 window"
                );
                self.contract = Some(CloudContract::V1);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// The user-catalog admission the root-unit join runs under: issued once
    /// per generation and renewed before it expires. Never installed on the
    /// host — no bridged stream runs under it — and fenced by being dropped
    /// when the generation is replaced.
    pub(super) fn ensure_catalog_permit(&mut self, clock: &dyn Clock) -> Result<(), Failure> {
        let now = clock.now_millis().ok_or(Failure::Unavailable)? / 1000;
        if self.catalog.as_ref().is_some_and(|catalog| {
            catalog.fresh(now) && catalog.permit.account_generation == self.account_generation
        }) {
            return Ok(());
        }
        let authority = self.current_authority()?;
        let token = self.authorized(|api, authority| {
            api.issue_token_v2(authority, CloudSyncScopeV2::UserCatalog)
        })?;
        if token.scope != CloudSyncScopeV2::UserCatalog
            || token.dataset.is_some()
            || Some(token.catalog_scope_id) != self.catalog_scope_id
        {
            return Err(Failure::DatasetMismatch);
        }
        let permit = self.permit_v2(&authority, &token, CloudPermitScopeV2::UserCatalog)?;
        if let Some(previous) = &self.catalog {
            tracing::info!(
                target: "koru_cloud",
                permit_id = %previous.permit.permit_id,
                "cloud pass: user-catalog admission replaced"
            );
        }
        let scoped = Scoped {
            credential: token.credential,
            expires_at_seconds: token.expires_at_seconds,
            route: token.route,
            permit,
            issued_under: self.receipt_generation(),
        };
        // A credential already inside its renewal window admits nothing:
        // the exchange waits for one that is live rather than move a byte
        // under a permit about to expire.
        let fresh = scoped.fresh(now);
        self.catalog = Some(scoped);
        if !fresh {
            return Err(Failure::CredentialExpired);
        }
        Ok(())
    }

    /// The root-unit join for this pass: the service's catalog against this
    /// device's eligible inventory, pulled (Profile units, then Workspace
    /// units) and pushed (the same order). A unit the Store refuses to admit
    /// or export is logged by id and skipped; a failed hosted call ends the
    /// exchange and is retried with the pass.
    pub(super) fn exchange_root_units(&mut self, clock: &dyn Clock) -> Result<(), Failure> {
        self.ensure_catalog_permit(clock)?;
        let account = self.current_authority()?.owner.account;
        let inventory = self.host.root_inventory()?;
        if inventory.held > 0 {
            tracing::info!(
                target: "koru_cloud",
                held = inventory.held,
                "cloud pass: root units held back from Cloud"
            );
        }
        self.local_units = inventory
            .units
            .iter()
            .map(|state| (unit_ref(state.unit), state.frontier.clone()))
            .collect();

        let mut remote = BTreeMap::new();
        let mut after = None;
        // Whether this loop read the service's **whole** catalog. Q10's
        // repair turns on it, so the loop says it rather than leaving it to
        // be inferred from control having reached the next line: a cancelled
        // pass returns from inside, and a `break` added later for any other
        // reason has to name its own answer.
        let complete = loop {
            if self.cancel.cancelled() {
                return Ok(());
            }
            let page = self.authorized(|api, authority| api.root_catalog(authority, after))?;
            for state in page.units {
                remote.insert(state.unit, state.frontier);
            }
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => break true,
            }
        };
        self.service_units = remote;

        // Pull: what Cloud holds that this device does not have yet, root
        // units only — personal units follow content.
        for kind in [
            CloudUnitKind::ProfileCatalog,
            CloudUnitKind::WorkspaceCatalog,
        ] {
            let units: Vec<(CloudUnitRefV2, Vec<u8>)> = self
                .service_units
                .iter()
                .filter(|(unit, _)| kind_of(**unit) == kind)
                .map(|(unit, frontier)| (*unit, frontier.clone()))
                .collect();
            for (unit, theirs) in units {
                if self.cancel.cancelled() {
                    return Ok(());
                }
                self.pull_unit(account, unit, &theirs, clock)?;
            }
        }
        // Q10's exit from the v1 window, after the pulls and before the push.
        // Here, and only here, three things are true at once: the service
        // answered the v2 contract, its whole catalog has been read, and any
        // unit it holds has already been joined. A Workspace this device holds
        // with no unit at all therefore proves no unit exists on the service
        // or here, and the unit Q7 withheld is published at once — so the
        // push below carries it in this same pass.
        self.repair_withheld_units(complete);
        // Push: what this device may export that Cloud does not hold at
        // this frontier.
        for kind in [
            CloudUnitKind::ProfileCatalog,
            CloudUnitKind::WorkspaceCatalog,
        ] {
            let units: Vec<(CloudUnitRefV2, Vec<u8>)> = self
                .local_units
                .iter()
                .filter(|(unit, _)| kind_of(**unit) == kind)
                .map(|(unit, frontier)| (*unit, frontier.clone()))
                .collect();
            for (unit, ours) in units {
                if self.cancel.cancelled() {
                    return Ok(());
                }
                self.push_unit(unit, &ours, clock)?;
            }
        }
        Ok(())
    }

    /// Publish the Workspace unit the v1 Cloud window withheld, for every
    /// Workspace this device holds with no unit at all (QUESTIONS.md Q10,
    /// case 1 of the decision; Q7 is what withheld it).
    ///
    /// The three conditions are all checked here rather than in the host, so
    /// the one place that knows them is the one that decides:
    ///
    /// - `complete` — the pagination loop ended because the next cursor was
    ///   absent. A partial read cannot establish that the service holds no
    ///   unit for a Workspace, only that none has been seen yet.
    /// - the service answered [`CloudContract::V2`]. The account-scope
    ///   binding is not a substitute: a 404 never unsets it, so a bound device
    ///   can still be running the v1 pair path for this generation.
    /// - the pass was not cancelled. A cancelled pass returns before the
    ///   pulls finish, so reaching here already implies it; saying it again
    ///   costs one comparison and makes a cancelled pass unable to repair
    ///   even if the loops above are ever restructured.
    ///
    /// A Store refusal is logged and the pass carries on, as every other
    /// Store refusal in this module is: the repair is a repair, and the next
    /// pass asks again under the same three conditions.
    fn repair_withheld_units(&mut self, complete: bool) {
        if !complete || self.contract != Some(CloudContract::V2) || self.cancel.cancelled() {
            return;
        }
        match self.host.repair_bootstrap_units() {
            Ok(outcome) => {
                if outcome.repaired == 0 {
                    return;
                }
                tracing::info!(
                    target: "koru_cloud",
                    repaired = outcome.repaired,
                    "cloud pass: root units withheld by the v1 window published"
                );
                // The push below reads `local_units`, which was captured
                // before the repair: without this the new units would wait a
                // whole pass for no reason.
                self.local_units = outcome
                    .inventory
                    .units
                    .iter()
                    .map(|state| (unit_ref(state.unit), state.frontier.clone()))
                    .collect();
            }
            Err(error) => {
                tracing::info!(
                    target: "koru_cloud",
                    code = error.code(),
                    "cloud pass: withheld root units not published"
                );
            }
        }
    }

    /// Personal units after content (§7.3): for every admitted Workspace
    /// with a user-personal admission, pull and push its personal unit.
    pub(super) fn exchange_personal_units(&mut self, clock: &dyn Clock) -> Result<(), Failure> {
        let account = self.current_authority()?.owner.account;
        let now = clock.now_millis().ok_or(Failure::Unavailable)? / 1000;
        let workspaces: Vec<WorkspaceId> = self
            .admissions
            .values()
            .filter(|admission| {
                admission
                    .personal
                    .as_ref()
                    .is_some_and(|personal| personal.fresh(now))
            })
            .map(|admission| admission.candidate.workspace_id)
            .collect();
        for workspace in workspaces {
            if self.cancel.cancelled() {
                return Ok(());
            }
            let unit = CloudUnitRefV2::PersonalV2 {
                subject_id: workspace,
            };
            if let Some(theirs) = self.service_units.get(&unit).cloned() {
                self.pull_unit(account, unit, &theirs, clock)?;
            }
            if let Some(ours) = self.local_units.get(&unit).cloned() {
                self.push_unit(unit, &ours, clock)?;
            }
        }
        Ok(())
    }

    /// The user-personal admission for one Workspace, when the account's
    /// catalog or this device holds a personal unit for it. Optional: a
    /// refusal is logged and the Workspace's content is admitted without
    /// it; it is never a reason to defer the Workspace.
    pub(super) fn admit_personal(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
    ) -> Option<Scoped> {
        let unit = CloudUnitRefV2::PersonalV2 {
            subject_id: workspace,
        };
        if !self.service_units.contains_key(&unit) && !self.local_units.contains_key(&unit) {
            return None;
        }
        let token = match self.authorized(|api, authority| {
            api.issue_token_v2(
                authority,
                CloudSyncScopeV2::UserPersonal {
                    workspace_id: workspace,
                },
            )
        }) {
            Ok(token) => token,
            Err(error) => {
                tracing::info!(
                    target: "koru_cloud",
                    workspace = ?workspace,
                    code = error.code(),
                    "cloud pass: personal admission refused"
                );
                return None;
            }
        };
        if token.dataset.is_some() || Some(token.catalog_scope_id) != self.catalog_scope_id {
            return None;
        }
        let permit = self
            .permit_v2(
                authority,
                &token,
                CloudPermitScopeV2::UserPersonal {
                    workspace_id: cloud_id(workspace.as_uuid()),
                },
            )
            .ok()?;
        Some(Scoped {
            credential: token.credential,
            expires_at_seconds: token.expires_at_seconds,
            route: token.route,
            permit,
            issued_under: self.receipt_generation(),
        })
    }

    /// The admission a unit's bytes may move under right now, or the
    /// refusal that stops them: the user-catalog admission for a root unit,
    /// the Workspace's user-personal admission for its personal unit. Both
    /// must be live and of this generation.
    fn gate(&mut self, unit: CloudUnitRefV2, clock: &dyn Clock) -> Result<(), Failure> {
        let now = clock.now_millis().ok_or(Failure::Unavailable)? / 1000;
        match unit {
            CloudUnitRefV2::ProfileCatalog { .. } | CloudUnitRefV2::WorkspaceCatalog { .. } => {
                self.ensure_catalog_permit(clock)
            }
            CloudUnitRefV2::PersonalV2 { subject_id } => {
                let live = self
                    .admissions
                    .get(&subject_id)
                    .and_then(|admission| admission.personal.as_ref())
                    .is_some_and(|personal| {
                        personal.fresh(now)
                            && personal.permit.account_generation == self.account_generation
                    });
                if live {
                    Ok(())
                } else {
                    Err(Failure::CredentialExpired)
                }
            }
        }
    }

    fn pull_unit(
        &mut self,
        account: KoruIdentityId,
        unit: CloudUnitRefV2,
        theirs: &[u8],
        clock: &dyn Clock,
    ) -> Result<(), Failure> {
        let ours = match self.host.root_unit_frontier(join_unit(unit)) {
            Ok(ours) => ours,
            Err(error) => {
                // The subject's mode blocks Cloud inbound (§4.4), or the
                // Store could not answer: nothing is pulled for it.
                tracing::info!(
                    target: "koru_cloud",
                    unit = ?unit,
                    code = error.code(),
                    "cloud pass: root unit not pulled"
                );
                return Ok(());
            }
        };
        if ours.as_deref() == Some(theirs) {
            return Ok(());
        }
        self.gate(unit, clock)?;
        let updates = self.authorized(|api, authority| {
            api.pull_root_unit(authority, unit, ours.clone().unwrap_or_default())
        })?;
        if updates.unit != unit {
            return Err(Failure::DatasetMismatch);
        }
        if updates.updates.is_empty() {
            return Ok(());
        }
        match self
            .host
            .join_root_unit(account, join_unit(unit), &updates.updates, Some(theirs))
        {
            Ok(CloudRootJoin::Applied { .. }) => {
                tracing::info!(target: "koru_cloud", unit = ?unit, "cloud pass: root unit joined");
            }
            Ok(CloudRootJoin::Pending { .. }) => {
                tracing::info!(
                    target: "koru_cloud",
                    unit = ?unit,
                    "cloud pass: root unit pending missing ancestors"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "koru_cloud",
                    unit = ?unit,
                    code = error.code(),
                    "cloud pass: root unit join refused"
                );
            }
        }
        Ok(())
    }

    /// Push one unit from the service's frontier. `Pending` is answered once
    /// more from the frontier the receipt names; `Held` is logged by unit id
    /// and nothing raw ever stands in for the held bytes.
    fn push_unit(
        &mut self,
        unit: CloudUnitRefV2,
        ours: &[u8],
        clock: &dyn Clock,
    ) -> Result<(), Failure> {
        let theirs = self.service_units.get(&unit).cloned();
        if theirs.as_deref() == Some(ours) {
            return Ok(());
        }
        self.gate(unit, clock)?;
        let mut recipient = theirs;
        for _ in 0..2 {
            let frames = match self
                .host
                .export_root_unit(join_unit(unit), recipient.as_deref())
            {
                Ok(CloudRootExport::Published {
                    frames,
                    redacted_ops,
                }) => {
                    if redacted_ops > 0 {
                        tracing::info!(
                            target: "koru_cloud",
                            unit = ?unit,
                            redacted_ops,
                            "cloud pass: root unit history redacted for Cloud"
                        );
                    }
                    frames
                }
                Ok(CloudRootExport::Held) => {
                    tracing::info!(target: "koru_cloud", unit = ?unit, "cloud pass: root unit held");
                    return Ok(());
                }
                Ok(CloudRootExport::NotFound) => return Ok(()),
                Err(error) => {
                    tracing::info!(
                        target: "koru_cloud",
                        unit = ?unit,
                        code = error.code(),
                        "cloud pass: root unit not exported"
                    );
                    return Ok(());
                }
            };
            if frames.is_empty() {
                return Ok(());
            }
            let receipt = self.authorized(|api, authority| {
                api.push_root_unit(
                    authority,
                    CloudRootUnitUpdatesV2 {
                        version: Version2,
                        unit,
                        updates: frames.clone(),
                        projection_watermark: 0,
                    },
                )
            })?;
            match receipt {
                CloudRootJoinReceiptV2::Applied {
                    frontier,
                    redacted_ops,
                    ..
                } => {
                    tracing::info!(
                        target: "koru_cloud",
                        unit = ?unit,
                        redacted_ops,
                        "cloud pass: root unit pushed"
                    );
                    self.service_units.insert(unit, frontier);
                    return Ok(());
                }
                CloudRootJoinReceiptV2::Unchanged { frontier, .. } => {
                    self.service_units.insert(unit, frontier);
                    return Ok(());
                }
                CloudRootJoinReceiptV2::Pending { frontier, .. } => {
                    tracing::info!(
                        target: "koru_cloud",
                        unit = ?unit,
                        "cloud pass: Cloud asked for the unit from its frontier"
                    );
                    recipient = Some(frontier);
                }
                CloudRootJoinReceiptV2::Held { reason, .. } => {
                    tracing::info!(
                        target: "koru_cloud",
                        unit = ?unit,
                        ?reason,
                        "cloud pass: Cloud held the unit"
                    );
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// The Store's binding as the wire declares it. Account ids are canonical
/// v4 by construction on both sides; a stored value that is not one is a
/// Store this pass cannot read, never a binding to declare.
fn declared_binding(binding: CloudAccountBindingRef) -> Result<CloudCatalogBindingRefV2, Failure> {
    fn parse<T: std::str::FromStr>(id: CloudId) -> Result<T, Failure> {
        id.as_uuid()
            .to_string()
            .parse()
            .map_err(|_| Failure::Unavailable)
    }
    Ok(CloudCatalogBindingRefV2 {
        account_id: parse(binding.account_id)?,
        catalog_scope_id: parse(binding.catalog_scope_id)?,
    })
}

fn kind_of(unit: CloudUnitRefV2) -> CloudUnitKind {
    match unit {
        CloudUnitRefV2::ProfileCatalog { .. } => CloudUnitKind::ProfileCatalog,
        CloudUnitRefV2::WorkspaceCatalog { .. } => CloudUnitKind::WorkspaceCatalog,
        CloudUnitRefV2::PersonalV2 { .. } => CloudUnitKind::PersonalV2,
    }
}
