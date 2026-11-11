// SPDX-License-Identifier: MPL-2.0

//! Local Profile metadata is available before peer enrollment or content.
//! A selection acknowledges durable local records, never an inferred peer grant.
use super::*;
use crate::enrollment::EnrollmentAuthorization;
use std::collections::BTreeSet;

impl ProductionAccountService {
    pub(super) fn provision_selected_profiles(
        &mut self,
        selected: &BTreeSet<ProfileId>,
        authority: &EnrollmentAuthorization,
        cancel: &Cancel,
    ) -> Result<(), ServiceError> {
        self.check_selection_authority(authority, cancel)?;
        if selected.is_empty() {
            return Ok(());
        }
        let authorization = AccountAuthorization::Bearer(&authority.access);
        let session = self
            .client
            .session(&authorization)
            .map_err(map_client_error)?;
        if session.session_id.to_string() != authority.session_id
            || session.koru_identity_id != authority.owner.account
            || session.auth_identity_id != authority.owner.auth_identity
            || session.device_id != Some(self.descriptor.device_id)
        {
            return Err(ServiceError::terminal("credential_owner_mismatch"));
        }
        let profiles = self
            .client
            .profiles(&authorization)
            .map_err(map_client_error)?
            .profiles;
        let inventory = self.local_profiles.inventory()?;
        let binding = format!("koru:{}", authority.owner.account);
        // Validate the entire set before any local commit. A row from another
        // owner or an identity without a current authorization is not importable.
        let mut initial = Vec::with_capacity(selected.len());
        // Chosen again after being removed from this computer, under this
        // same account: brought back before it is provisioned. Restored
        // only after the whole set validates, as every other local commit is.
        let mut restore = Vec::new();
        for profile in selected {
            let mut matches = profiles.iter().filter(|row| row.profile_id == *profile);
            let row = matches
                .next()
                .ok_or_else(|| ServiceError::terminal("profile_not_authorized"))?;
            if matches.next().is_some()
                || row.koru_identity_id != authority.owner.account
                || !row
                    .auth_identities
                    .iter()
                    .any(|identity| identity.auth_identity_id == authority.owner.auth_identity)
            {
                return Err(ServiceError::terminal("profile_not_authorized"));
            }
            for local in inventory
                .profiles
                .iter()
                .filter(|row| row.profile_id == *profile)
            {
                if local.account_binding != binding {
                    return Err(ServiceError::terminal("profile_account_conflict"));
                }
                // A Profile this computer removed is still this account's,
                // and the person just picked it. It used to be refused here
                // as `profile_removed`, which no surface explained, so
                // *Connect selected* did nothing a person could see.
                if local.removed_at_millis.is_some() {
                    restore.push(*profile);
                }
            }
            initial.push((
                *profile,
                koru_store_wire::ProvisionAccountProfile {
                    identity_id: authority.owner.account.to_string(),
                    // The account's copy seeding a local row that does not exist
                    // yet — the one job it still has. The identity document
                    // replaces this the moment it arrives (`commit_profile_identity`),
                    // and `provision_account_profile` leaves an existing row's
                    // name alone, so this never overwrites a synced name.
                    name: row
                        .name_snapshot
                        .clone()
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| "Profile".into()),
                    locale: row.locale_snapshot.clone(),
                    created_at_millis: self
                        .clock
                        .now_millis()
                        .ok_or_else(|| ServiceError::retryable("clock_unavailable"))?,
                },
            ));
        }
        for profile in restore {
            self.check_selection_authority(authority, cancel)?;
            self.local_profiles.restore_removed(profile)?;
        }
        for (profile, initial) in initial {
            self.check_selection_authority(authority, cancel)?;
            // Store compare-and-set is the final fence against concurrent
            // removal or rebinding. Earlier successful members remain usable
            // if a later local commit fails; the set is not acknowledged yet.
            self.local_profiles.provision(profile, &initial)?;
        }
        self.check_selection_authority(authority, cancel)?;
        self.profiles = profiles;
        Ok(())
    }

    fn check_selection_authority(
        &self,
        authority: &EnrollmentAuthorization,
        cancel: &Cancel,
    ) -> Result<(), ServiceError> {
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        let current = self
            .enrollment_authorization()
            .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?;
        if current.owner != authority.owner
            || current.namespace != authority.namespace
            || current.record_version != authority.record_version
            || current.session_id != authority.session_id
            || current.access != authority.access
        {
            return Err(ServiceError::terminal("credential_owner_mismatch"));
        }
        Ok(())
    }
}
