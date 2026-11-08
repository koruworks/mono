// SPDX-License-Identifier: MPL-2.0

//! The workspace-management namespace: the catalog a manager lists, and the
//! one operation that removes a workspace.
//!
//! **Two calls and a listing, and the two calls are one conversation.** The
//! surface asks [`CatalogClient::confirmation`] what an action would do,
//! shows the answer, and — if the person accepts — sends the token back with
//! [`CatalogClient::lifecycle`]. That round trip is what makes the
//! acceptance be about the consequence the gateway computed rather than about
//! whatever the caller says it displayed (ADR-267).
//!
//! **Everything here is blocking**, like the rest of this crate. A window calls
//! it off its event thread — `koru-workspaces-ui`'s link is the worker that
//! does so — because a destructive operation closes terminals and reaches a
//! Store, and neither is a thing to do inside a frame.

use koru_api_wire::{
    WorkspaceConfirmation, WorkspaceConfirmationRequest, WorkspaceLifecycleAction,
    WorkspaceLifecycleOutcome, WorkspaceLifecycleRequest, WorkspaceMigration,
    WorkspaceMigrationConfirmationRequest, WorkspaceMigrationOutcome, WorkspaceMigrationRequest,
};
use koru_store_client::http::RetryPolicy;
use koru_store_core::model::Workspace;

use crate::Client;

mod removal;
#[cfg(test)]
mod tests;

/// Why a management call did not do what was asked.
#[derive(Debug)]
pub enum WorkspaceError {
    /// Discovery, authentication, connection, or response decoding.
    Transport(koru_store_core::errors::Error),
    /// A structured refusal from the management routes.
    Api(WorkspaceApiFailure),
}

#[derive(Debug)]
pub struct WorkspaceApiFailure {
    pub status: u16,
    /// `permission_denied`, `invalid_request`, `not_found`,
    /// `capability_unavailable`, `workspace_sessions_not_closed`,
    /// `workspace_lifecycle_incomplete`.
    pub code: String,
    pub message: String,
    /// **What the operation had done when it refused.**
    ///
    /// Present exactly when the refusal is about the operation rather than
    /// about reaching it, and the fields are the ones a success carries — S24
    /// is `sessions_closed: true` with `mutation_applied: false`, and a surface
    /// reporting it needs the list of terminals it closed, not a sentence
    /// saying some were.
    pub outcome: Option<WorkspaceLifecycleOutcome>,
    /// The same, for a migration.
    ///
    /// **A second field rather than an untyped one.** The two outcomes are two
    /// public wire shapes — a lifecycle's names an action, a migration's names
    /// two profiles — and only one of them ever parses out of a given body. A
    /// single field holding whichever fitted would leave a caller unable to say
    /// which operation it was reading, and a caller that guessed wrong would
    /// read a partial failure as no failure at all.
    pub migration: Option<WorkspaceMigrationOutcome>,
    /// **What refused underneath.** The Store's own code, when the operation
    /// reached the Store and the Store said no. `workspace_in_use` is retained
    /// for a structured answer from an older gateway that did not yet apply
    /// the confirmed room-disconnect contract.
    pub cause: Option<String>,
    /// Whether sending the same thing again could ever work.
    pub retryable: bool,
}

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

/// The Store's refusal when a workspace still has document connections.
///
/// Named here rather than matched as a literal at a call site, because a
/// surface that has to *say* what is wrong is the only reason this code
/// travels this far.
pub const WORKSPACE_IN_USE: &str = "workspace_in_use";

impl WorkspaceError {
    /// Whether the session simply may not do this.
    ///
    /// The presentation case: a window that was not vouched for shows the
    /// destructive rows disabled with a reason rather than hiding them.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Api(failure) if failure.code == "permission_denied")
    }

    /// **S24, recognised.** The terminals are closed and the catalog is
    /// unchanged — the one refusal a surface reports as a state of the
    /// workspace rather than as a failed attempt.
    pub fn partial_outcome(&self) -> Option<&WorkspaceLifecycleOutcome> {
        match self {
            Self::Api(failure) => failure
                .outcome
                .as_ref()
                .filter(|outcome| outcome.sessions_closed && !outcome.mutation_applied),
            Self::Transport(_) => None,
        }
    }

    /// **S24 for a move**: the terminals are closed and the workspace has not
    /// moved. The same state of affairs as [`Self::partial_outcome`], about the
    /// other operation, and reported the same way — as a fact about where the
    /// workspace is, not as a failed attempt.
    pub fn partial_migration(&self) -> Option<&WorkspaceMigrationOutcome> {
        match self {
            Self::Api(failure) => failure
                .migration
                .as_ref()
                .filter(|outcome| outcome.sessions_closed && !outcome.mutation_applied),
            Self::Transport(_) => None,
        }
    }

    /// Whether an older gateway refused because the workspace had open
    /// documents. Current gateways disconnect document rooms after explicit
    /// soft-delete confirmation; retaining the typed cause keeps mixed-version
    /// failures honest instead of flattening them.
    pub fn documents_connected(&self) -> bool {
        matches!(
            self,
            Self::Api(failure)
                if failure.code == WORKSPACE_IN_USE
                    || failure.cause.as_deref() == Some(WORKSPACE_IN_USE)
        )
    }

    /// Whether sending the same request again could work.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api(failure) => failure.retryable,
            // A connection that failed before an answer is exactly the case a
            // retry is for.
            Self::Transport(_) => true,
        }
    }
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::Api(failure) => write!(formatter, "{}: {}", failure.code, failure.message),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// The typed workspace-management namespace.
pub struct CatalogClient<'a> {
    pub(crate) client: &'a Client,
}

impl CatalogClient<'_> {
    /// **Every workspace, lifecycle states included.**
    ///
    /// The including-deleted projection is the manager's list and not an
    /// optional extra: archived (S17) and soft-deleted (S18) rows are what it
    /// exists to show, and the ordinary listing hides both. One fetched
    /// authority feeds every group the surface draws — there is no second
    /// call for the trash.
    pub fn workspaces(&self) -> WorkspaceResult<Vec<Workspace>> {
        self.client
            .store()
            .workspaces_including_deleted()
            .map_err(WorkspaceError::Transport)
    }

    /// **Ask the gateway whether this session may remove a workspace.**
    ///
    /// A window asks for `workspace.destroy`; the gateway decides, and the
    /// hello it answers with does not say what it decided. So the surface asks,
    /// once, when it opens — because the alternative is offering Archive and
    /// Delete to a person who will be refused when they press them, and a
    /// control that is going to say no should say so before it is pressed
    /// rather than after.
    ///
    /// **It asks about a workspace that cannot exist**, so the answer is only
    /// ever about the permission. The scope is checked before the id is looked
    /// up, so a session without it is told `permission_denied` and one with it
    /// is told there is no such workspace — and nothing is issued either way,
    /// because a token is minted only after a real workspace resolves.
    ///
    /// *Recommendation, recorded:* the honest home for this is a `scopes` field
    /// on the gateway hello, so a session knows what it was granted without
    /// asking a question it does not care about the answer to. That is a wire
    /// change beyond SP-8b's seam; this is the version that needs none.
    pub fn may_destroy(&self) -> bool {
        const NOBODY: &str = "00000000-0000-4000-8000-000000000000";
        !matches!(
            self.confirmation(NOBODY, WorkspaceLifecycleAction::Archive),
            Err(error) if error.is_permission_denied()
        )
    }

    /// **Ask what an action would do, and get the token that proves it was
    /// shown.**
    ///
    /// Called when the confirmation opens, not when it is accepted: the answer
    /// is what the confirmation says (S13). The token is single-use and bound
    /// to this workspace and this action, so a dialog that is cancelled simply
    /// leaves one unspent.
    pub fn confirmation(
        &self,
        workspace: &str,
        action: WorkspaceLifecycleAction,
    ) -> WorkspaceResult<WorkspaceConfirmation> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/workspaces/{workspace}/lifecycle/confirmation"),
                &WorkspaceConfirmationRequest { action },
                // Issuing is a read with a receipt: it changes no workspace, so
                // a replayed one produces another token and nothing else.
                RetryPolicy::Safe,
            )
            .map_err(map_public)
    }

    /// **Archive, soft-delete or restore, closing the workspace's terminals
    /// first.**
    ///
    /// `confirmation` is the token from [`Self::confirmation`], unchanged. The
    /// answer says what the operation did in the order it did it; a refusal
    /// that carries an outcome says the same about how far it got, which is
    /// how S24 reaches a surface as a fact rather than as an error string.
    pub fn lifecycle(
        &self,
        workspace: &str,
        action: WorkspaceLifecycleAction,
        confirmation: &str,
    ) -> WorkspaceResult<WorkspaceLifecycleOutcome> {
        self.lifecycle_replacing(
            workspace,
            action,
            confirmation,
            &koru_api_wire::default_workspace_name(),
        )
    }

    pub fn lifecycle_replacing(
        &self,
        workspace: &str,
        action: WorkspaceLifecycleAction,
        confirmation: &str,
        replacement_name: &str,
    ) -> WorkspaceResult<WorkspaceLifecycleOutcome> {
        self.lifecycle_with_attempt(workspace, action, confirmation, replacement_name, None)
    }

    pub fn lifecycle_for_removal(
        &self,
        workspace: &str,
        confirmation: &str,
        attempt: koru_store_core::ids::RemovalAttemptId,
    ) -> WorkspaceResult<WorkspaceLifecycleOutcome> {
        self.lifecycle_with_attempt(
            workspace,
            WorkspaceLifecycleAction::SoftDelete,
            confirmation,
            "",
            Some(attempt),
        )
    }

    fn lifecycle_with_attempt(
        &self,
        workspace: &str,
        action: WorkspaceLifecycleAction,
        confirmation: &str,
        replacement_name: &str,
        removal_attempt: Option<koru_store_core::ids::RemovalAttemptId>,
    ) -> WorkspaceResult<WorkspaceLifecycleOutcome> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/workspaces/{workspace}/lifecycle"),
                &WorkspaceLifecycleRequest {
                    action,
                    confirmation: confirmation.to_owned(),
                    replacement_name: replacement_name.to_owned(),
                    removal_attempt,
                },
                // **Never replayed by the transport.** The operation is
                // idempotent *as a whole request a caller repeats*, but the
                // token is spent on the first attempt — so a transport-level
                // replay would send a confirmation the gateway has already
                // consumed and turn a completed operation into a refusal. The
                // retry is the surface's, with a fresh confirmation.
                RetryPolicy::AuthenticationOnly,
            )
            .map_err(map_public)
    }

    /// **Ask the gateway whether this session may move a workspace.**
    ///
    /// [`Self::may_destroy`]'s question about the other scope, asked the same
    /// way and for the same reason — and it is a separate question, because
    /// the two scopes are separate: a window can hold either, both, or
    /// neither, and a surface that assumed one implied the other would offer a
    /// row that is going to refuse.
    ///
    /// The workspace it asks about cannot exist, so the answer is only ever
    /// about the permission: the scope is checked before the id is looked up.
    /// The destination is the same nonexistent id, which never gets consulted.
    pub fn may_migrate(&self) -> bool {
        const NOBODY: &str = "00000000-0000-4000-8000-000000000000";
        !matches!(
            self.migration_confirmation(NOBODY, NOBODY),
            Err(error) if error.is_permission_denied()
        )
    }

    /// **Ask what moving a workspace to a profile would close, and get the
    /// token that proves it was shown.**
    ///
    /// The answer names both ends of the move and the terminals it would close.
    /// The token is bound to this workspace *and this destination*: choosing a
    /// different profile in the picker is a different question, and asks again.
    pub fn migration_confirmation(
        &self,
        workspace: &str,
        to: &str,
    ) -> WorkspaceResult<WorkspaceMigration> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/workspaces/{workspace}/migration/confirmation"),
                &WorkspaceMigrationConfirmationRequest { to: to.to_owned() },
                RetryPolicy::Safe,
            )
            .map_err(map_public)
    }

    /// **Give a workspace to another profile, closing its terminals first.**
    ///
    /// `confirmation` is the token from [`Self::migration_confirmation`],
    /// unchanged, and `to` is the destination it was issued for — sending a
    /// different one is a 400 rather than a move nobody agreed to.
    pub fn migrate(
        &self,
        workspace: &str,
        to: &str,
        confirmation: &str,
    ) -> WorkspaceResult<WorkspaceMigrationOutcome> {
        self.migrate_replacing(
            workspace,
            to,
            confirmation,
            &koru_api_wire::default_workspace_name(),
        )
    }

    pub fn migrate_replacing(
        &self,
        workspace: &str,
        to: &str,
        confirmation: &str,
        replacement_name: &str,
    ) -> WorkspaceResult<WorkspaceMigrationOutcome> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/workspaces/{workspace}/migration"),
                &WorkspaceMigrationRequest {
                    to: to.to_owned(),
                    confirmation: confirmation.to_owned(),
                    replacement_name: replacement_name.to_owned(),
                },
                // Never replayed by the transport, for [`Self::lifecycle`]'s
                // reason: the token is spent on the first attempt.
                RetryPolicy::AuthenticationOnly,
            )
            .map_err(map_public)
    }
}

fn map_public(error: koru_store_client::http::PublicError) -> WorkspaceError {
    match error {
        koru_store_client::http::PublicError::Transport(error) => WorkspaceError::Transport(error),
        koru_store_client::http::PublicError::Failure(failure) => {
            let body = failure.body.unwrap_or_default();
            WorkspaceError::Api(WorkspaceApiFailure {
                status: failure.status,
                code: failure.code,
                message: failure.message,
                outcome: serde_json::from_value(body["outcome"].clone()).ok(),
                migration: serde_json::from_value(body["outcome"].clone()).ok(),
                cause: body["cause"]
                    .as_str()
                    .filter(|cause| !cause.is_empty())
                    .map(str::to_owned),
                retryable: body["retryable"].as_bool().unwrap_or(false),
            })
        }
    }
}
