// SPDX-License-Identifier: MPL-2.0

//! The hosted half of the coordinator: the typed native account client.
//!
//! It is deliberately thin. Every route already exists and is already
//! strict; the only judgment here is which refusal is a reason to retry, and
//! that a 503 `server_unavailable` from the descriptor read means "not ready
//! yet", never "make something up". A 404 on a v2 catalog route means the
//! service does not speak v2 yet (root-catalog Plan 3.2's window), and the
//! two binding refusals keep their own words because they are terminal. So
//! do the seven refusals of QUESTIONS.md Q12: a refusal the service worded
//! is answered as that refusal, never flattened into an outage and retried
//! for ever.

use koru_account_client::{AccountAuthorization, AccountClient, ClientError};
use koru_account_wire::cloud_sync::{SyncTokenRequestV1, SyncTokenResponseV1};
use koru_account_wire::{
    CloudCatalogBindingRequestV2, CloudDiscoveryScopeV2, CloudProfileSelectionRequestV1,
    CloudRootUnitRequestV2, CloudWorkspaceCursorV1, CloudWorkspaceDescriptorV1,
    CloudWorkspacePageV1, ErrorCode, JwksResponseV1, SyncTokenRequestV2, Version1, Version2,
};

use super::*;

/// Wraps the shared typed client. Cloning the client is cheap and the worker
/// owns its own handle, so no lock is held across an HTTP call.
pub struct HostedApi {
    client: AccountClient,
}

impl HostedApi {
    pub fn new(client: AccountClient) -> Self {
        Self { client }
    }
}

impl Api for HostedApi {
    fn discover(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudWorkspaceCursorV1>,
    ) -> Result<CloudWorkspacePageV1, Failure> {
        self.client
            .cloud_workspaces(
                after.as_ref(),
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn select(
        &mut self,
        authority: &EnrollmentAuthorization,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) -> Result<(), Failure> {
        self.client
            .select_profile_cloud(
                profile,
                &CloudProfileSelectionRequestV1 {
                    version: Version1,
                    workspace_id: workspace,
                },
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map(|_| ())
            .map_err(failure)
    }

    fn describe(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        profile: ProfileId,
    ) -> Result<CloudWorkspaceDescriptorV1, Failure> {
        self.client
            .cloud_workspace_descriptor(
                workspace,
                profile,
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn issue_token(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        profile: ProfileId,
    ) -> Result<SyncTokenResponseV1, Failure> {
        self.client
            .issue_cloud_sync_token(
                workspace,
                &SyncTokenRequestV1 {
                    version: Version1,
                    profile_id: profile,
                },
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn jwks(&mut self) -> Result<JwksResponseV1, Failure> {
        self.client.jwks().map_err(failure)
    }

    fn bind_catalog(
        &mut self,
        authority: &EnrollmentAuthorization,
        held: Option<CloudCatalogBindingRefV2>,
    ) -> Result<CloudCatalogBindingV2, Failure> {
        self.client
            .bind_cloud_catalog(
                &CloudCatalogBindingRequestV2 {
                    version: Version2,
                    held,
                },
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn root_catalog(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudUnitRefV2>,
    ) -> Result<CloudRootCatalogV2, Failure> {
        self.client
            .cloud_root_catalog(
                after.as_ref(),
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn pull_root_unit(
        &mut self,
        authority: &EnrollmentAuthorization,
        unit: CloudUnitRefV2,
        frontier: Vec<u8>,
    ) -> Result<CloudRootUnitUpdatesV2, Failure> {
        self.client
            .pull_cloud_root_unit(
                &CloudRootUnitRequestV2 {
                    version: Version2,
                    unit,
                    frontier,
                },
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn push_root_unit(
        &mut self,
        authority: &EnrollmentAuthorization,
        updates: CloudRootUnitUpdatesV2,
    ) -> Result<CloudRootJoinReceiptV2, Failure> {
        self.client
            .push_cloud_root_unit(&updates, &AccountAuthorization::Bearer(&authority.access))
            .map_err(failure)
    }

    fn discover_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudDiscoveryCursorV2>,
    ) -> Result<CloudDiscoveryReceiptV2, Failure> {
        self.client
            .cloud_discovery(
                CloudDiscoveryScopeV2::Workspaces,
                after.as_ref(),
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn issue_token_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        scope: CloudSyncScopeV2,
    ) -> Result<SyncTokenResponseV2, Failure> {
        self.client
            .issue_cloud_sync_token_v2(
                &SyncTokenRequestV2 {
                    version: Version2,
                    scope,
                },
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }

    fn describe_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
    ) -> Result<CloudWorkspaceDescriptorV2, Failure> {
        self.client
            .cloud_workspace_descriptor_v2(
                workspace,
                &AccountAuthorization::Bearer(&authority.access),
            )
            .map_err(failure)
    }
}

/// A transport problem is offline and retried; a revoked session or a denied
/// principal is not, because only a new authorization can change it. A 404
/// is a route the service does not serve, whether or not it was answered in
/// Koru's words — the live v1 service answers the binding route from its own
/// framework, not with a `DeveloperErrorV1` — and the two binding refusals
/// are terminal for the account and keep their words. Nothing *but* a 404 is
/// read as a missing route: every other codeless status keeps the reading it
/// had while the status was being thrown away.
///
/// **A refusal the service worded is not an outage** (QUESTIONS.md Q12).
/// Seven of its own codes used to fall through to [`Failure::Unavailable`]
/// and be retried for ever, which is how a person ends up watching an
/// unavailable line that can never tell them anything. Each now says what it
/// is: a unit this build is too old to read (a), the three refusals of one
/// unit that no retry can change (b), the binding precondition the client
/// can satisfy itself (c), and service-side damage that is retried but never
/// called an outage (d). Generic integrity faults stay on
/// `server_unavailable`, so these seven are distinguishable by code alone —
/// the status is never what tells them apart, and **only** a 401
/// `unauthenticated` ends a session.
fn failure(error: ClientError) -> Failure {
    match error {
        ClientError::Transport(_) => Failure::Offline,
        ClientError::RateLimited { retry_after_millis } => {
            Failure::RateLimited(Duration::from_millis(retry_after_millis))
        }
        ClientError::Api { status, code } => match (status, code.as_str()) {
            (_, CATALOG_BINDING_CONFLICT) => Failure::CatalogBindingConflict,
            (_, CATALOG_BINDING_MISMATCH) => Failure::CatalogBindingMismatch,
            // Q12, before the status arms: a coded refusal says what it is,
            // and a 403 or 404 carrying one of these codes is that refusal
            // rather than a denied principal or a missing route.
            (_, ROOT_UNIT_FORMAT_TOO_NEW) => Failure::UnitFormatTooNew,
            (_, ROOT_UNIT_INVALID) => Failure::UnitRefused(UnitRefusal::UnitInvalid),
            (_, ROOT_EXPORT_REFUSED) => Failure::UnitRefused(UnitRefusal::ExportRefused),
            (_, ROOT_UNIT_ADDRESS_INVALID) => Failure::UnitRefused(UnitRefusal::AddressInvalid),
            (_, ROOT_PLACEMENT_FOREIGN) => Failure::UnitRefused(UnitRefusal::PlacementForeign),
            (_, ROOT_WORKSPACE_UNIT_REQUIRED) => {
                Failure::UnitRefused(UnitRefusal::WorkspaceUnitRequired)
            }
            (_, CATALOG_BINDING_REQUIRED) => Failure::CatalogBindingRequired,
            (_, ROOT_PROJECTION_CORRUPT) => Failure::AccountDamaged(AccountDamage::RootProjection),
            (_, CATALOG_BINDING_CORRUPT) => Failure::AccountDamaged(AccountDamage::CatalogBinding),
            (401, _) => Failure::SessionExpired,
            (403, _) => Failure::Denied,
            (404, _) => Failure::RouteNotFound,
            (_, ErrorCode::SERVER_UNAVAILABLE) => Failure::NotReady,
            _ => Failure::Unavailable,
        },
        ClientError::ApiWithoutCode { status: 404 } => Failure::RouteNotFound,
        ClientError::ApiWithoutCode { .. }
        | ClientError::InvalidRequest
        | ClientError::MissingBrowserOrigin
        | ClientError::InvalidResponse
        | ClientError::ContractMismatch => Failure::Unavailable,
    }
}

/// Build the credential verifier a host dials with, from the JWKS the
/// coordinator read under the current account generation. Only the
/// capability trust domain is admitted: access and attestation keys are never
/// unioned in, and an empty capability set is a deployment without Cloud sync
/// rather than a reason to try another domain.
pub fn capability_verifier(
    region: koru_account_core::workspace_authorization::Region,
    issuer: &str,
    keys: &JwksResponseV1,
) -> Result<koru_account_session::TokenCapabilityVerifier, Failure> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    if keys.capability_keys.is_empty() || keys.capability_keys.len() > 16 {
        return Err(Failure::Unavailable);
    }
    let mut verifying = Vec::with_capacity(keys.capability_keys.len());
    for key in &keys.capability_keys {
        if key.kty != "OKP"
            || key.crv != "Ed25519"
            || key.key_use != "sig"
            || key.alg != "EdDSA"
            || key.x.len() != 43
        {
            return Err(Failure::Unavailable);
        }
        let bytes: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&key.x)
            .map_err(|_| Failure::Unavailable)?
            .try_into()
            .map_err(|_| Failure::Unavailable)?;
        if URL_SAFE_NO_PAD.encode(bytes) != key.x {
            return Err(Failure::Unavailable);
        }
        let public =
            ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(|_| Failure::Unavailable)?;
        verifying.push((key.kid.clone(), public));
    }
    koru_account_session::TokenCapabilityVerifier::new(region, issuer.to_owned(), verifying)
        .map_err(|_| Failure::Unavailable)
}
