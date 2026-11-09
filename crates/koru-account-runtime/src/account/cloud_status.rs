// SPDX-License-Identifier: MPL-2.0

//! Progress is scoped to the chosen account and Profile, never proof of coverage.
//!
//! A permit's Profile is the placement hint it carries (root-catalog Plan
//! 3.2): a Workspace's record counts toward the Profile it is currently
//! placed under, a Profile-units record toward its own Profile, and a
//! Workspace that is explicitly unassigned toward no Profile row at all.
use crate::cloud::{Snapshot, Stage};
use koru_agent_wire::{AccountState, CloudConnectionStage};

pub(super) fn project(state: &mut AccountState, snapshot: &Snapshot) {
    if snapshot.owner_scope == Some(state.active_scope) && snapshot.account_id.is_none() {
        for profile in &mut state.cloud_profiles {
            profile.discovery = None;
        }
    }
    if snapshot.owner_scope != Some(state.active_scope)
        || snapshot.account_id.map(|id| id.to_string()) != state.active_account_id
    {
        return;
    }
    for profile in &mut state.cloud_profiles {
        profile.discovery = snapshot
            .discovery
            .as_ref()
            .filter(|receipt| Some(receipt.account_id) == snapshot.account_id)
            .and_then(|receipt| {
                koru_account_core::ProfileId::parse(&profile.profile_id)
                    .map(|id| receipt.for_profile(id))
            });
        let names = |candidate: Option<koru_account_core::ProfileId>| {
            candidate.is_some_and(|id| id.to_string() == profile.profile_id)
        };
        let records: Vec<_> = snapshot
            .records
            .iter()
            .filter(|record| names(record.profile_id))
            .collect();
        profile.stage = if (snapshot.records.is_empty() && snapshot.failure_code.is_some())
            || snapshot.deferred.iter().any(|row| names(row.profile_id))
            || records.iter().any(|row| {
                row.error_code.is_some() || matches!(row.stage, Stage::Offline | Stage::Unavailable)
            }) {
            CloudConnectionStage::Retrying
        } else if records
            .iter()
            .any(|row| row.stage == Stage::WaitingForConsent)
        {
            CloudConnectionStage::WaitingForConsent
        } else if records.iter().any(|row| {
            matches!(
                row.stage,
                Stage::Reconciling | Stage::CheckingCoverage | Stage::Settled
            )
        }) {
            CloudConnectionStage::Syncing
        } else {
            CloudConnectionStage::Connecting
        };
    }
}

#[cfg(test)]
mod tests;
