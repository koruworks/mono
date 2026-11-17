// SPDX-License-Identifier: MPL-2.0

//! *Live in the account, in the Trash here*
//! (`docs/features/trash-recovery/PLAN.final.md` P2 to P4).
//!
//! A Workspace can be in this device's Trash while the person's account
//! still holds it live under one of their Profiles, and stay that way: its
//! mode here closes the Cloud path, so its deletion never leaves and the
//! account's placement never arrives (root-catalog TECHNICAL.final §4.4).
//! Nothing on the device said so. This is the one fact that does:
//!
//! ```text
//! store_state  root_catalog.account_live.<workspace>
//!              { account, profile, first_seen_ms, acknowledged_ms }
//! ```
//!
//! **Written from one observation and replaced as a set.** After each
//! complete discovery the Agent hands the Store every Workspace the account
//! lists live under a Profile on this device
//! ([`Store::reconcile_account_live`]). The Store keeps a fact for the ones
//! in the Trash here whose deletion cannot leave, and drops the facts that
//! account no longer supports, in one transaction.
//!
//! **A deletion that can still leave is not this case.** On a syncing device
//! a Workspace somebody just trashed is live in the account only until the
//! next pass delivers the deletion. Marking it would be offering to undo
//! what the person asked for, so it is never marked — and nothing is held
//! back on its account. *Cannot leave* is the test the export inventory
//! makes for the Cloud path (`export_source_or_hold`): the mode closes it,
//! the facts are still in the private overlay, or there is no durable unit
//! to send. The pass asks after its own inventory read, so the publication
//! job has already had its chance at any overlay that could be published.
//!
//! **The fact counts only while its Profile is here and is that account's**
//! ([`Store::account_live_hold`]). That is asked wherever the fact is read
//! rather than tidied by each way a Profile can leave: a Profile detached,
//! removed or deleted makes the fact inert, and the next observation
//! replaces the set.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::ports::RootPublication;
use crate::profile_account::ProfileAccountBinding;
use crate::root_catalog::CatalogUnitRef;
use crate::transport::TransportPath;

/// The `store_state` key of one Workspace's fact.
pub(super) fn account_live_key(workspace: WorkspaceId) -> String {
    format!("root_catalog.account_live.{}", workspace.to_storage())
}

/// What the Store holds about one such Workspace. Ids and times only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLiveFact {
    /// The account that lists the Workspace live.
    pub account: Uuid,
    /// The Profile that account keeps it under.
    pub profile: ProfileId,
    /// When this device first learned of it, in epoch milliseconds.
    pub first_seen_ms: i64,
    /// When the person was told, once they have been.
    #[serde(default)]
    pub acknowledged_ms: Option<i64>,
}

/// One Workspace that is in the Trash here and live in the account, as the
/// recovery surfaces list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrashedInAccount {
    pub workspace: Workspace,
    /// The Profile the account keeps it under: where *Restore* puts it.
    pub profile: ProfileId,
    pub first_seen_ms: i64,
    pub acknowledged_ms: Option<i64>,
}

/// What one observation came to ([`Store::reconcile_account_live`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountLiveOutcome {
    /// The listed Workspaces that are in the Trash here: admitted for
    /// nothing.
    pub trashed: BTreeSet<WorkspaceId>,
    /// Whether the set of facts changed, so the surfaces that show it read
    /// again. An observation that confirms what was known changes nothing.
    pub changed: bool,
}

fn encode(fact: &AccountLiveFact) -> Result<String> {
    serde_json::to_string(fact)
        .map_err(|_| Error::new(Code::CorruptState, "an account-live fact cannot be written"))
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// The fact held for `workspace`, whether or not it still counts.
    fn account_live_fact(&self, workspace: WorkspaceId) -> Result<Option<AccountLiveFact>> {
        self.catalog
            .state(&account_live_key(workspace))?
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|_| Error::new(Code::CorruptState, "invalid account-live fact"))
            })
            .transpose()
    }

    /// The fact for `workspace` **if it counts**: its Profile is active on
    /// this device and bound to the account that said so. Every reader asks
    /// this — the listing, the purge — so a Profile that left by any road
    /// stops the fact without anything having to clear it.
    pub(super) fn account_live_hold(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<AccountLiveFact>> {
        let Some(fact) = self.account_live_fact(workspace)? else {
            return Ok(None);
        };
        let Some(profile) = self.catalog.profile(fact.profile)? else {
            return Ok(None);
        };
        let bound = ProfileAccountBinding::parse_identity_id(&fact.account.to_string())
            .is_ok_and(|binding| profile.account.as_deref() == Some(binding.to_storage().as_str()));
        Ok((bound && !profile.is_holder()).then_some(fact))
    }

    /// Replace what this device knows of `account`'s live Workspaces that
    /// are in the Trash here. `listed` is every Workspace that account lists
    /// live under a Profile on this device, with that Profile.
    ///
    /// Answers the listed Workspaces that are in the Trash here — all of
    /// them, so the caller admits none of them for content. A fact is kept
    /// only for those whose deletion cannot leave over the Cloud path (see
    /// the module note); one already held keeps when it was first seen and
    /// whether the person was told. Facts of this account for Workspaces no
    /// longer listed, no longer in the Trash, or now able to leave are
    /// removed. Another account's facts are not touched.
    pub fn reconcile_account_live(
        &mut self,
        account: Uuid,
        listed: &[(WorkspaceId, ProfileId)],
        now: Timestamp,
    ) -> Result<AccountLiveOutcome> {
        let listed: BTreeMap<WorkspaceId, ProfileId> = listed.iter().copied().collect();
        let mut trashed = BTreeSet::new();
        let mut state = Vec::new();
        for row in self.catalog.workspaces_including_deleted()? {
            let under = listed.get(&row.id).copied();
            let held = self
                .account_live_fact(row.id)?
                .filter(|fact| fact.account == account);
            // Most rows are neither listed by this account nor marked for
            // it: nothing to decide, and no unit to open on their behalf.
            if under.is_none() && held.is_none() {
                continue;
            }
            let wanted = match under {
                Some(profile) if self.effective_deleted_at(row.id, row.deleted_at)?.is_some() => {
                    trashed.insert(row.id);
                    let cannot_leave = self
                        .export_source_or_hold(
                            CatalogUnitRef::Workspace { workspace: row.id },
                            TransportPath::Cloud,
                        )?
                        .is_none();
                    cannot_leave.then_some(profile)
                }
                _ => None,
            };
            match (wanted, held) {
                (Some(profile), Some(fact)) if fact.profile == profile => {}
                (Some(profile), held) => state.push((
                    account_live_key(row.id),
                    Some(encode(&AccountLiveFact {
                        account,
                        profile,
                        // Moved to another Profile in the account: the same
                        // Workspace, first seen when it was, told again.
                        first_seen_ms: held.map_or(now.millis(), |fact| fact.first_seen_ms),
                        acknowledged_ms: None,
                    })?),
                )),
                (None, Some(_)) => state.push((account_live_key(row.id), None)),
                (None, None) => {}
            }
        }
        let changed = !state.is_empty();
        if changed {
            let scope = self.catalog.local_user_scope()?;
            let facts = state.len();
            self.catalog.publish_root(
                scope,
                &RootPublication {
                    units: Vec::new(),
                    projections: Vec::new(),
                    state,
                },
            )?;
            tracing::info!(
                facts,
                "root lifecycle: account-live facts for trashed workspaces replaced"
            );
        }
        Ok(AccountLiveOutcome { trashed, changed })
    }

    /// Every Workspace in the Trash here that the account holds live, for
    /// the recovery surfaces: a fact that counts, on a Workspace still in
    /// the Trash. Catalog order.
    pub fn trashed_in_account(&self) -> Result<Vec<TrashedInAccount>> {
        let mut found = Vec::new();
        for row in self.catalog.workspaces_including_deleted()? {
            if self.effective_deleted_at(row.id, row.deleted_at)?.is_none() {
                continue;
            }
            if let Some(fact) = self.account_live_hold(row.id)? {
                found.push(TrashedInAccount {
                    workspace: row,
                    profile: fact.profile,
                    first_seen_ms: fact.first_seen_ms,
                    acknowledged_ms: fact.acknowledged_ms,
                });
            }
        }
        Ok(found)
    }

    /// Record that the person was told about `workspace`, so they are told
    /// once. Answers whether a fact was there to mark; marking one already
    /// marked keeps the first time.
    pub fn acknowledge_trashed_in_account(
        &mut self,
        workspace: WorkspaceId,
        now: Timestamp,
    ) -> Result<bool> {
        let Some(mut fact) = self.account_live_fact(workspace)? else {
            return Ok(false);
        };
        if fact.acknowledged_ms.is_none() {
            fact.acknowledged_ms = Some(now.millis());
            self.catalog
                .set_state(&account_live_key(workspace), &encode(&fact)?)?;
        }
        Ok(true)
    }
}
