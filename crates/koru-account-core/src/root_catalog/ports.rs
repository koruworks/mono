// SPDX-License-Identifier: MPL-2.0

//! The storage seams the private service implements for the hosted root
//! catalog. Policy is in the sibling modules; these traits carry rows.
//! Every identifier a repository trusts arrives from the verified session,
//! never from a request body, as the v1 ports already insist.

use crate::{CatalogScopeId, KoruIdentityId, PortFuture, WorkspaceId};

use super::CloudUnitRef;
use super::admission::Admitted;
use super::binding::{AccountCatalogBinding, BindingRefusal, BindingRequest};
use super::placement::HostedContentFacts;
use super::projection::{DiscoveryProjection, UnitFrontier};

/// One admitted unit as stored: the derived snapshot, the compare-and-set
/// revision, and how far the projection has read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredUnit {
    pub unit: CloudUnitRef,
    pub snapshot: Vec<u8>,
    pub revision: i64,
    /// The revision the discovery projection has committed for this unit;
    /// `revision` once the projection is current, lower while a job is
    /// outstanding, 0 before the first projection.
    pub projection_watermark: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogRepositoryError {
    /// The compare-and-set revision did not match: another device committed
    /// first. The caller stages again against the new row.
    Conflict,
    NotFound,
    Unavailable,
    Corrupt,
    Refused(BindingRefusal),
}

impl CatalogRepositoryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Unavailable => "server_unavailable",
            Self::Corrupt => "authorization_corrupt",
            Self::Refused(refusal) => refusal.code(),
        }
    }
}

/// The hosted catalog's rows: binding, units, projection.
pub trait CloudCatalogRepository: Send + Sync {
    /// Get or create the account's binding under [`super::decide_binding`],
    /// inside one transaction with a unique key on the account id; a lost
    /// insert race answers the row that won.
    fn bind_account_catalog<'a>(
        &'a self,
        request: BindingRequest,
    ) -> PortFuture<'a, Result<AccountCatalogBinding, CatalogRepositoryError>>;

    /// The scope's row for `unit`, if admitted.
    fn admitted_unit<'a>(
        &'a self,
        scope: CatalogScopeId,
        unit: CloudUnitRef,
    ) -> PortFuture<'a, Result<Option<StoredUnit>, CatalogRepositoryError>>;

    /// Every admitted unit of the scope with its revision: the inventory a
    /// `CloudRootCatalogV2` is built from and the input to coverage.
    fn admitted_units<'a>(
        &'a self,
        scope: CatalogScopeId,
    ) -> PortFuture<'a, Result<Vec<UnitFrontier>, CatalogRepositoryError>>;

    /// Commit an [`Admitted::Stored`] outcome: insert or replace exactly
    /// `expected_revision`, stamping `revision + 1`. Any other `Admitted`
    /// arm is a programming error and is refused as `Conflict` without a
    /// write. Returns the row as stored.
    fn commit_admitted_unit<'a>(
        &'a self,
        scope: CatalogScopeId,
        admitted: Admitted,
    ) -> PortFuture<'a, Result<StoredUnit, CatalogRepositoryError>>;

    /// The committed discovery projection of the scope.
    fn projection<'a>(
        &'a self,
        scope: CatalogScopeId,
    ) -> PortFuture<'a, Result<DiscoveryProjection, CatalogRepositoryError>>;

    /// Commit a projection advanced by the reducer and the unit watermarks
    /// it moved, in one transaction. A watermark never regresses; a commit
    /// that would is refused as `Conflict`.
    fn commit_projection<'a>(
        &'a self,
        scope: CatalogScopeId,
        projection: DiscoveryProjection,
        watermarks: Vec<UnitFrontier>,
    ) -> PortFuture<'a, Result<(), CatalogRepositoryError>>;
}

/// Content facts for v2 admission: the Workspace's identity, lifecycle and
/// the asking account's membership, read coherently. No Profile is asked
/// for and none is answered.
pub trait CloudContentAuthority: Send + Sync {
    fn content_facts<'a>(
        &'a self,
        account: KoruIdentityId,
        workspace: WorkspaceId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<Option<HostedContentFacts>, CatalogRepositoryError>>;
}
