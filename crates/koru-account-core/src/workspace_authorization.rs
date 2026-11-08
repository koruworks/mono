// SPDX-License-Identifier: MPL-2.0

//! Regional workspace authorization, independent of credentials and storage.
//!
//! Authentication adapters verify credentials; authority adapters read current
//! hosted membership. Neither request DTOs nor local peer trust are authority.
//! `WorkspaceService` combines those adapters through one policy function.
//! Local/offline Store operations do not enter this regional boundary.

use koru_store_core::WorkspaceId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AccountSessionId, AppConnectionId, DeviceId, KoruIdentityId, OpaqueSecret, PortFuture,
};

/// Residency is explicit even when only the US service is deployed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Region {
    Us,
    Eu,
}

/// Audit identity, not a credential. Constructing this value grants nothing.
/// Service, replica and connection identities cannot masquerade as a
/// registered device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Principal {
    UserSession {
        account_id: KoruIdentityId,
        session_id: AccountSessionId,
    },
    Device {
        account_id: KoruIdentityId,
        device_id: DeviceId,
    },
    CloudReplica {
        replica_id: Uuid,
        workspace_id: WorkspaceId,
    },
    KoruService {
        service_id: Uuid,
    },
    /// A third-party application's approved connection to one account, such
    /// as an AI assistant reaching the account's synced workspaces through
    /// Koru's hosted MCP server. It is not a device: it has no `DeviceId`, no
    /// pairing and no trust. What it may reach is the connection's own
    /// resolved workspace set, which the authority port reads live.
    AppConnection {
        account_id: KoruIdentityId,
        connection_id: AppConnectionId,
    },
}

impl Principal {
    pub fn account_id(self) -> Option<KoruIdentityId> {
        match self {
            Self::UserSession { account_id, .. }
            | Self::Device { account_id, .. }
            | Self::AppConnection { account_id, .. } => Some(account_id),
            Self::CloudReplica { .. } | Self::KoruService { .. } => None,
        }
    }
}

/// Purpose-specific scopes; admin and write do not implicitly include read.
/// Object, comment and PAT permissions arrive with their consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceScope {
    #[serde(rename = "workspace:read")]
    WorkspaceRead,
    #[serde(rename = "workspace:write")]
    WorkspaceWrite,
    #[serde(rename = "documents:read")]
    DocumentsRead,
    #[serde(rename = "documents:write")]
    DocumentsWrite,
    #[serde(rename = "tasks:read")]
    TasksRead,
    #[serde(rename = "tasks:write")]
    TasksWrite,
    #[serde(rename = "workspace:admin")]
    WorkspaceAdmin,
    #[serde(rename = "workspace:sync")]
    WorkspaceSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAction {
    ReadWorkspace,
    UpdateWorkspace,
    ReadDocument,
    CreateDocument,
    MutateDocument,
    ReadTasks,
    CreateTask,
    UpdateTask,
    EnableCloud,
    DisableCloud,
    DeleteCloud,
    Replicate,
}

impl WorkspaceAction {
    fn required_scope(self) -> WorkspaceScope {
        use WorkspaceScope::*;
        match self {
            Self::ReadWorkspace => WorkspaceRead,
            Self::UpdateWorkspace => WorkspaceWrite,
            Self::ReadDocument => DocumentsRead,
            Self::CreateDocument | Self::MutateDocument => DocumentsWrite,
            Self::ReadTasks => TasksRead,
            Self::CreateTask | Self::UpdateTask => TasksWrite,
            Self::EnableCloud | Self::DisableCloud | Self::DeleteCloud => WorkspaceAdmin,
            Self::Replicate => WorkspaceSync,
        }
    }

    fn requires_owner(self) -> bool {
        matches!(
            self,
            Self::EnableCloud | Self::DisableCloud | Self::DeleteCloud
        )
    }
}

/// What a person approved for one app connection: read, or read and edit.
/// The spelling is the connection row's and the connection sync credential's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionAccess {
    Read,
    ReadWrite,
}

impl AppConnectionAccess {
    /// The workspace scopes an app connection's evidence and grant carry for
    /// its non-replication actions. Tasks follow the document scopes: a
    /// connection that may read documents may read tasks, and one that may
    /// edit documents may change tasks. Write never implies read here either;
    /// `ReadWrite` lists both. Replication is authorized by the connection's
    /// sync credential (`workspace:sync`), not by these.
    pub const fn scopes(self) -> &'static [WorkspaceScope] {
        use WorkspaceScope::*;
        match self {
            Self::Read => &[WorkspaceRead, DocumentsRead, TasksRead],
            Self::ReadWrite => &[
                WorkspaceRead,
                DocumentsRead,
                TasksRead,
                DocumentsWrite,
                TasksWrite,
            ],
        }
    }
}

/// These values must come from verified credential claims, never request JSON.
/// There is deliberately no Deserialize implementation. The trusted
/// `WorkspaceAuthenticator` adapter owns signature, expiry, revocation and
/// credential-purpose verification before returning this evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAuthentication {
    pub principal: Principal,
    pub region: Region,
    pub issuer: String,
    pub audience: String,
    pub scopes: Vec<WorkspaceScope>,
    /// None is permitted for account-wide regional API credentials only.
    /// Verifiers must preserve all signed resource restrictions here.
    pub binding: Option<WorkspaceCredentialBinding>,
}

/// A sync credential is indivisibly bound to all three signed restrictions.
/// A regional API credential can be restricted to a workspace without naming
/// a replica; neither form may silently shed its membership epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceCredentialBinding {
    Workspace {
        workspace_id: WorkspaceId,
        membership_epoch: u64,
    },
    Sync {
        workspace_id: WorkspaceId,
        membership_epoch: u64,
        replica_id: Uuid,
    },
}

/// Exact subject/resource binding from the regional authority. A collaborator
/// may have admin scope, but only the hosted owner controls Cloud lifecycle.
/// Plan 11 supplies transactional membership and lifecycle reads, including
/// first-claim possession proof. This structure is not persisted or cached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceGrant {
    pub principal: Principal,
    pub workspace_id: WorkspaceId,
    pub region: Region,
    pub owner_account_id: KoruIdentityId,
    pub scopes: Vec<WorkspaceScope>,
    pub membership_epoch: u64,
    /// The authoritative logical replica assignment, required for replication.
    pub replica_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceRequest {
    pub workspace_id: WorkspaceId,
    pub action: WorkspaceAction,
}

/// Exactly one deployment's accepted authority. URLs/audiences are configured,
/// so isolated stacks and the explicitly bounded issuer rollout can use the
/// same policy. Credential compatibility belongs to the authentication adapter;
/// the policy never guesses aliases or accepts another region's issuer.
#[derive(Clone, Debug)]
pub struct RegionalAuthorization {
    region: Region,
    issuer: String,
    api_audience: String,
    sync_audience: String,
}

impl RegionalAuthorization {
    pub fn new(
        region: Region,
        issuer: String,
        api_audience: String,
        sync_audience: String,
    ) -> Result<Self, InvalidRegionalAuthorization> {
        if [&issuer, &api_audience, &sync_audience]
            .iter()
            .any(|value| value.trim().is_empty())
            || api_audience == sync_audience
        {
            return Err(InvalidRegionalAuthorization);
        }
        Ok(Self {
            region,
            issuer,
            api_audience,
            sync_audience,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRegionalAuthorization;

/// Each code below has a producer in the decision function or service ports.
/// Serialize the code unchanged on every transport and status surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRefusal {
    AuthenticationRequired,
    AuthorizationUnavailable,
    WrongRegion,
    WrongIssuer,
    WrongAudience,
    PrincipalNotAllowed,
    WorkspaceNotAuthorized,
    OwnerRequired,
    InsufficientScope,
    SyncBindingRequired,
    MembershipEpochMismatch,
    ReplicaMismatch,
}

impl WorkspaceRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthorizationUnavailable => "authorization_unavailable",
            Self::WrongRegion => "wrong_region",
            Self::WrongIssuer => "wrong_issuer",
            Self::WrongAudience => "wrong_audience",
            Self::PrincipalNotAllowed => "principal_not_allowed",
            Self::WorkspaceNotAuthorized => "workspace_not_authorized",
            Self::OwnerRequired => "owner_required",
            Self::InsufficientScope => "insufficient_scope",
            Self::SyncBindingRequired => "sync_binding_required",
            Self::MembershipEpochMismatch => "membership_epoch_mismatch",
            Self::ReplicaMismatch => "replica_mismatch",
        }
    }
}

/// Content-free audit identity is returned for both permission and refusal.
/// An unverified credential has no principal, even if it claims one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDecision {
    pub principal: Option<Principal>,
    pub request: WorkspaceRequest,
    pub result: Result<(), WorkspaceRefusal>,
}

/// The single policy function. Pure and fakeable: no I/O, logging or clocks.
/// Inputs are evidence from trusted adapters, not evidence of authentication by
/// themselves. Consumers authorize through `WorkspaceService`, not this helper.
pub fn decide_workspace_access(
    regional: &RegionalAuthorization,
    authentication: &WorkspaceAuthentication,
    grant: Option<&WorkspaceGrant>,
    request: WorkspaceRequest,
) -> WorkspaceDecision {
    let principal = authentication.principal;
    let result = (|| {
        if authentication.region != regional.region {
            return Err(WorkspaceRefusal::WrongRegion);
        }
        if authentication.issuer != regional.issuer {
            return Err(WorkspaceRefusal::WrongIssuer);
        }
        let audience = if request.action == WorkspaceAction::Replicate {
            &regional.sync_audience
        } else {
            &regional.api_audience
        };
        if authentication.audience != *audience {
            return Err(WorkspaceRefusal::WrongAudience);
        }
        let principal_allowed = match principal {
            Principal::UserSession { .. } => request.action != WorkspaceAction::Replicate,
            Principal::Device { .. } => true,
            Principal::CloudReplica { .. } => request.action == WorkspaceAction::Replicate,
            // No rung-1 service consumer has a workspace permission yet.
            Principal::KoruService { .. } => false,
            // Reads, edits and replication of its own resolved set. Never
            // workspace metadata changes and never the Cloud lifecycle. Which
            // of its writes go through is the connection's access, carried in
            // its scopes ([`AppConnectionAccess::scopes`]).
            Principal::AppConnection { .. } => !matches!(
                request.action,
                WorkspaceAction::UpdateWorkspace
                    | WorkspaceAction::EnableCloud
                    | WorkspaceAction::DisableCloud
                    | WorkspaceAction::DeleteCloud
            ),
        };
        if !principal_allowed {
            return Err(WorkspaceRefusal::PrincipalNotAllowed);
        }
        if let Principal::CloudReplica { workspace_id, .. } = principal {
            if workspace_id != request.workspace_id {
                return Err(WorkspaceRefusal::WorkspaceNotAuthorized);
            }
        }
        let grant = grant.ok_or(WorkspaceRefusal::WorkspaceNotAuthorized)?;
        if grant.principal != principal || grant.workspace_id != request.workspace_id {
            return Err(WorkspaceRefusal::WorkspaceNotAuthorized);
        }
        if grant.region != regional.region {
            return Err(WorkspaceRefusal::WrongRegion);
        }
        if let Some(binding) = authentication.binding {
            let (workspace_id, membership_epoch) = match binding {
                WorkspaceCredentialBinding::Workspace {
                    workspace_id,
                    membership_epoch,
                }
                | WorkspaceCredentialBinding::Sync {
                    workspace_id,
                    membership_epoch,
                    ..
                } => (workspace_id, membership_epoch),
            };
            if workspace_id != request.workspace_id {
                return Err(WorkspaceRefusal::WorkspaceNotAuthorized);
            }
            if membership_epoch != grant.membership_epoch {
                return Err(WorkspaceRefusal::MembershipEpochMismatch);
            }
        }
        if request.action == WorkspaceAction::Replicate {
            let Some(WorkspaceCredentialBinding::Sync { replica_id, .. }) = authentication.binding
            else {
                return Err(WorkspaceRefusal::SyncBindingRequired);
            };
            if grant.replica_id != Some(replica_id) {
                return Err(WorkspaceRefusal::ReplicaMismatch);
            }
            if let Principal::CloudReplica {
                replica_id: identity,
                ..
            } = principal
            {
                if identity != replica_id {
                    return Err(WorkspaceRefusal::ReplicaMismatch);
                }
            }
        }
        if request.action.requires_owner() && principal.account_id() != Some(grant.owner_account_id)
        {
            return Err(WorkspaceRefusal::OwnerRequired);
        }
        let scope = request.action.required_scope();
        if !authentication.scopes.contains(&scope) || !grant.scopes.contains(&scope) {
            return Err(WorkspaceRefusal::InsufficientScope);
        }
        Ok(())
    })();
    WorkspaceDecision {
        principal: Some(principal),
        request,
        result,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceAuthenticationError {
    Rejected,
    Unavailable,
}

/// Trusted application composition installs this port. Never select an adapter
/// from request data. It must validate credentials before constructing evidence.
pub trait WorkspaceAuthenticator: Send + Sync {
    fn authenticate<'a>(
        &'a self,
        credential: &'a OpaqueSecret,
    ) -> PortFuture<'a, Result<WorkspaceAuthentication, WorkspaceAuthenticationError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceAuthorityUnavailable;

pub trait WorkspaceAuthority: Send + Sync {
    /// Read live regional authority for this exact principal/resource. Missing
    /// or revoked membership returns None, never a synthetic owner grant.
    fn read_grant<'a>(
        &'a self,
        principal: Principal,
        workspace_id: WorkspaceId,
    ) -> PortFuture<'a, Result<Option<WorkspaceGrant>, WorkspaceAuthorityUnavailable>>;
}

/// Admission result created only by the service, not from caller-supplied
/// Principal/claims. It is point-in-time evidence, not a reusable capability.
/// The mutating consumer must fence its commit against `membership_epoch` and
/// its own lifecycle/Cloud-access epoch in the same transaction (Plan 11).
#[derive(Debug)]
pub struct AuthorizedWorkspaceAction {
    decision: WorkspaceDecision,
    membership_epoch: u64,
    replica_id: Option<Uuid>,
}

impl AuthorizedWorkspaceAction {
    pub fn decision(&self) -> &WorkspaceDecision {
        &self.decision
    }

    pub fn membership_epoch(&self) -> u64 {
        self.membership_epoch
    }

    pub fn replica_id(&self) -> Option<Uuid> {
        self.replica_id
    }
}

/// Regional domain admission boundary. Future REST, native and MCP adapters
/// enter here; domain implementations keep Loro semantics and event production.
/// This foundation installs no route and does not replace local Store admission.
pub struct WorkspaceService<'a> {
    regional: &'a RegionalAuthorization,
    authenticator: &'a dyn WorkspaceAuthenticator,
    authority: &'a dyn WorkspaceAuthority,
}

impl<'a> WorkspaceService<'a> {
    pub fn new(
        regional: &'a RegionalAuthorization,
        authenticator: &'a dyn WorkspaceAuthenticator,
        authority: &'a dyn WorkspaceAuthority,
    ) -> Self {
        Self {
            regional,
            authenticator,
            authority,
        }
    }

    pub async fn authorize(
        &self,
        credential: &OpaqueSecret,
        request: WorkspaceRequest,
    ) -> Result<AuthorizedWorkspaceAction, WorkspaceDecision> {
        let authentication =
            self.authenticator
                .authenticate(credential)
                .await
                .map_err(|error| WorkspaceDecision {
                    principal: None,
                    request,
                    result: Err(match error {
                        WorkspaceAuthenticationError::Rejected => {
                            WorkspaceRefusal::AuthenticationRequired
                        }
                        WorkspaceAuthenticationError::Unavailable => {
                            WorkspaceRefusal::AuthorizationUnavailable
                        }
                    }),
                })?;
        let grant = self
            .authority
            .read_grant(authentication.principal, request.workspace_id)
            .await
            .map_err(|_| WorkspaceDecision {
                principal: Some(authentication.principal),
                request,
                result: Err(WorkspaceRefusal::AuthorizationUnavailable),
            })?;
        let decision =
            decide_workspace_access(self.regional, &authentication, grant.as_ref(), request);
        match (&decision.result, grant) {
            (Ok(()), Some(grant)) => Ok(AuthorizedWorkspaceAction {
                decision,
                membership_epoch: grant.membership_epoch,
                replica_id: grant.replica_id,
            }),
            _ => Err(decision),
        }
    }
}

#[cfg(test)]
mod tests;
