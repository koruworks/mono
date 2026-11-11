// SPDX-License-Identifier: MPL-2.0

//! The v1 pair path, kept for the compatibility window of root-catalog
//! Plan 3.2 only: a service that answers the binding route with 404 still
//! speaks `discover` / `select` / `describe` / `issue_token`, and this is
//! that path unchanged in what it asks the service — selection, the Profile
//! presence check and the Profile-first bootstrap included, because the v1
//! descriptor cannot be read without its Profile.
//!
//! What changed is what it produces: the same per-scope admission the v2
//! path produces (a Workspace content permit, and the pair's Profile dataset
//! read as a Profile-units admission the first admitted Workspace under the
//! Profile lends to it), so everything after admission — sessions, status,
//! fences, the host — is one machinery, not two.

use super::*;

impl<A: Api, H: Host> Coordinator<A, H> {
    /// `GET /v1/account/cloud/workspaces`, paged. The membership Profile is
    /// read as the current placement, which is what the service answers it
    /// from during the window (the contract's §4).
    pub(super) fn enumerate_v1(&mut self) -> Result<VecDeque<Candidate>, Failure> {
        let mut after = None;
        let mut candidates = VecDeque::new();
        loop {
            if self.cancel.cancelled() {
                break;
            }
            let page = self.authorized(|api, authority| api.discover(authority, after))?;
            for candidate in &page.workspaces {
                let candidate = Candidate::from_v1(candidate);
                self.discovery_counts
                    .entry(candidate.placement)
                    .or_default()
                    .discovered += 1;
                if candidates.len() >= MAX_CARRIED_CANDIDATES {
                    tracing::warn!(
                        capacity = MAX_CARRIED_CANDIDATES,
                        "cloud subscriptions: account exceeds standing capacity"
                    );
                    return Err(Failure::Unavailable);
                }
                candidates.push_back(candidate);
            }
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => {
                    self.discovery_exhausted = true;
                    break;
                }
            }
        }
        Ok(candidates)
    }

    /// Steps 3 to 7 of the pair path for one Workspace.
    pub(super) fn admit_v1(
        &mut self,
        authority: &EnrollmentAuthorization,
        candidate: Candidate,
    ) -> Result<Option<Admission>, Failure> {
        let profile = candidate.legacy_profile.ok_or(Failure::DatasetMismatch)?;
        let workspace = candidate.workspace_id;

        // Idempotent, once per pass. A 403 denies this pair for the pass.
        self.authorized(|api, authority| api.select(authority, profile, workspace))?;
        if self.cancel.cancelled() {
            return Ok(None);
        }

        // The credential first: the originating-device read names the exact
        // issued Workspace dataset (HOST-CONTROL §3.1 `read_cloud_bootstrap`),
        // so the host answers for that replica and not for a Workspace id
        // alone. A token wasted on a refused bootstrap is cheaper than a
        // bootstrap read that had to guess a replica.
        let mut token =
            self.authorized(|api, authority| api.issue_token(authority, workspace, profile))?;
        let identity = match self
            .host
            .local_workspace(authority.owner.account, token.workspace().dataset())?
        {
            Some(local) => {
                if local.workspace_id != workspace {
                    return Err(Failure::DatasetMismatch);
                }
                LegacyIdentity::Local(local)
            }
            // A 503 is "not ready": skip the Workspace this pass and retry
            // with backoff rather than inventing an identity for it.
            None => LegacyIdentity::Hosted(
                self.authorized(|api, authority| api.describe(authority, workspace, profile))?,
            ),
        };

        // Local content does not prove active Profile presence: removal keeps
        // retained identity data. The v1 descriptor is the Profile's.
        self.host
            .ensure_profile_presence(profile, authority.owner.account)
            .map_err(bootstrap_failure)?;
        let hosted = matches!(identity, LegacyIdentity::Hosted(_));
        if self.cancel.cancelled() {
            return Ok(None);
        }

        let mut descriptor = descriptor_v1(&candidate, profile, &identity, &token)?;
        if let Err(Failure::DatasetMismatch) =
            correlate_v1(&candidate, profile, &descriptor, &token)
        {
            // One bounded re-read and reissue. Selection can invalidate
            // epochs; values from two authority generations are never mixed.
            token =
                self.authorized(|api, authority| api.issue_token(authority, workspace, profile))?;
            let identity = match self
                .host
                .local_workspace(authority.owner.account, token.workspace().dataset())?
            {
                Some(local) => LegacyIdentity::Local(local),
                None => LegacyIdentity::Hosted(
                    self.authorized(|api, authority| api.describe(authority, workspace, profile))?,
                ),
            };
            descriptor = descriptor_v1(&candidate, profile, &identity, &token)?;
            correlate_v1(&candidate, profile, &descriptor, &token)?;
        }

        let expires_at_seconds = token.expires_at_seconds();
        let account = cloud_id(authority.owner.account.as_uuid());
        let content_permit = CloudPermitV2 {
            version: koru_sync_control::cloud::CLOUD_PERMIT_VERSION_V2,
            permit_id: self.mint(),
            account_id: account,
            catalog_scope_id: None,
            scope: CloudPermitScopeV2::Workspace {
                descriptor: control_descriptor_v1(&descriptor),
            },
            account_generation: self.account_generation,
            authority_generation: 1,
            expires_at_seconds,
        };
        content_permit
            .validate()
            .map_err(|_| Failure::DatasetMismatch)?;
        if hosted {
            // Profile-first bootstrap for a Workspace this device does not
            // hold: a v1 service has no root units to join, so the host
            // seeds the placement from the descriptor's Profile, as it did
            // before M3 (the permit says so by its missing catalog scope).
            self.host
                .ensure_workspace_presence(&content_permit)
                .map_err(bootstrap_failure)?;
        }
        let profile_permit = CloudPermitV2 {
            version: koru_sync_control::cloud::CLOUD_PERMIT_VERSION_V2,
            permit_id: self.mint(),
            account_id: account,
            catalog_scope_id: None,
            scope: CloudPermitScopeV2::ProfileUnits {
                dataset: token.profile().dataset(),
                // Identity and Private of the authorizing Workspace's
                // Profile. The v1 Personal unit is not here: the Store has
                // answered `None` for its address since catalog 13, and in
                // v2 personal data is the Workspace-keyed scope.
                allowed_profile_units: vec![
                    ProfileUnitRef::Identity { profile },
                    ProfileUnitRef::Private { profile },
                ],
                // The pair's credential, which this permit shares with the
                // Workspace's, is issued under this Workspace.
                authority_workspace_id: Some(cloud_id(workspace.as_uuid())),
            },
            account_generation: self.account_generation,
            authority_generation: 1,
            expires_at_seconds,
        };
        profile_permit
            .validate()
            .map_err(|_| Failure::DatasetMismatch)?;
        Ok(Some(Admission {
            candidate,
            content: Scoped {
                credential: token.credential().clone(),
                expires_at_seconds,
                route: route_of(token.workspace()),
                permit: content_permit,
                issued_under: 1,
            },
            personal: None,
            legacy_profile_units: Some(Scoped {
                credential: token.credential().clone(),
                expires_at_seconds,
                route: route_of(token.profile()),
                permit: profile_permit,
                issued_under: 1,
            }),
        }))
    }
}

/// The identity a v1 Workspace permit is built from, and where it came from.
enum LegacyIdentity {
    Local(LocalBootstrap),
    Hosted(CloudWorkspaceDescriptorV1),
}

/// The identity-only v1 descriptor the Workspace permit carries. For the
/// originating device it is the durable local manifest id plus the current
/// candidate and credential context (HOST-CONTROL §3.1); nothing is
/// invented when neither source has it.
fn descriptor_v1(
    candidate: &Candidate,
    profile: ProfileId,
    identity: &LegacyIdentity,
    token: &SyncTokenResponseV1,
) -> Result<CloudWorkspaceDescriptorV1, Failure> {
    match identity {
        LegacyIdentity::Hosted(descriptor) => Ok(*descriptor),
        LegacyIdentity::Local(local) => {
            let CloudReplicaPrincipalV1::Workspace {
                workspace_id,
                incarnation_id,
                logical_replica_id,
            } = token.workspace().dataset()
            else {
                return Err(Failure::DatasetMismatch);
            };
            if workspace_id != cloud_id(candidate.workspace_id.as_uuid())
                || incarnation_id != cloud_id(candidate.incarnation)
            {
                return Err(Failure::DatasetMismatch);
            }
            Ok(CloudWorkspaceDescriptorV1 {
                version: koru_account_wire::Version1,
                profile_id: profile,
                workspace_id: local.workspace_id,
                workspace_incarnation_id: candidate.incarnation,
                logical_replica_id: Uuid::from_bytes(*logical_replica_id.as_bytes()),
                lineage_id: candidate.incarnation,
                manifest_document_id: local.manifest_document_id,
                authorization_version: candidate.authorization_version,
                membership_epoch: candidate.membership_epoch,
                cloud_access_epoch: candidate.cloud_access_epoch,
            })
        }
    }
}

/// The issued dataset principals must be the ones the descriptor and the
/// candidate already name. A mismatch is refused, never reconciled by
/// taking one field from each side.
fn correlate_v1(
    candidate: &Candidate,
    profile: ProfileId,
    descriptor: &CloudWorkspaceDescriptorV1,
    token: &SyncTokenResponseV1,
) -> Result<(), Failure> {
    if descriptor.workspace_id != candidate.workspace_id
        || descriptor.profile_id != profile
        || descriptor.workspace_incarnation_id != candidate.incarnation
        || descriptor.lineage_id != candidate.incarnation
    {
        return Err(Failure::DatasetMismatch);
    }
    let CloudReplicaPrincipalV1::Workspace {
        workspace_id,
        incarnation_id,
        logical_replica_id,
    } = token.workspace().dataset()
    else {
        return Err(Failure::DatasetMismatch);
    };
    if workspace_id != cloud_id(descriptor.workspace_id.as_uuid())
        || incarnation_id != cloud_id(descriptor.workspace_incarnation_id)
        || logical_replica_id != cloud_id(descriptor.logical_replica_id)
    {
        return Err(Failure::DatasetMismatch);
    }
    let CloudReplicaPrincipalV1::Profile { profile_id, .. } = token.profile().dataset() else {
        return Err(Failure::DatasetMismatch);
    };
    if profile_id != cloud_id(descriptor.profile_id.as_uuid()) {
        return Err(Failure::DatasetMismatch);
    }
    Ok(())
}

/// The v1 wire descriptor as the control protocol's v2 descriptor: the
/// membership Profile becomes the placement hint at revision 0.
fn control_descriptor_v1(descriptor: &CloudWorkspaceDescriptorV1) -> ControlDescriptorV2 {
    ControlDescriptorV2::from_v1(&koru_sync_control::cloud::CloudWorkspaceDescriptorV1 {
        version: koru_sync_control::cloud::CLOUD_WORKSPACE_DESCRIPTOR_VERSION,
        profile_id: cloud_id(descriptor.profile_id.as_uuid()),
        workspace_id: cloud_id(descriptor.workspace_id.as_uuid()),
        workspace_incarnation_id: cloud_id(descriptor.workspace_incarnation_id),
        logical_replica_id: cloud_id(descriptor.logical_replica_id),
        lineage_id: cloud_id(descriptor.lineage_id),
        manifest_document_id: cloud_id(descriptor.manifest_document_id),
        authorization_version: descriptor.authorization_version,
        membership_epoch: descriptor.membership_epoch,
        cloud_access_epoch: descriptor.cloud_access_epoch,
    })
}
