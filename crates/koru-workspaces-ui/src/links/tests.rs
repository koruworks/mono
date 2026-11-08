// SPDX-License-Identifier: MPL-2.0

//! What the link says about itself, and what a controller on it starts as.
//!
//! **Nothing here dials.** `WorkspaceLinks::open` spawns a thread that connects
//! to a Gateway, and a suite that ran it would be asking whatever installation
//! the machine has running to hand out a destructive scope. Every constructor
//! under test is [`WorkspaceLinks::inert`], which differs from the real one only
//! in that.

use super::*;
use crate::{Authorization, WorkspacesController};

#[test]
fn answers_are_queued_before_the_wake_including_late_settlements() {
    let (tx, rx) = channel();
    let seen = std::cell::Cell::new(0);
    let wake = || {
        assert!(matches!(
            rx.try_recv(),
            Ok(Answer::Settled(crate::CommandId(7), _))
        ));
        seen.set(seen.get() + 1);
    };
    assert!(publish_answer(
        &tx,
        &wake,
        Answer::Settled(crate::CommandId(7), Box::new(Answer::Unavailable))
    ));
    assert_eq!(seen.get(), 1);
}

#[test]
fn catalog_invalidations_are_visible_before_the_wake() {
    let flags = CatalogFlags::default();
    let seen = std::cell::Cell::new(0);
    let wake = || {
        assert!(flags.take(CatalogChange::Workspaces));
        seen.set(seen.get() + 1);
    };
    invalidate_and_wake(&flags, &wake, CatalogChange::Workspaces);
    assert_eq!(seen.get(), 1);
}

#[test]
fn worker_exit_settles_every_queued_command_once() {
    let links = WorkspaceLinks::open_ended();
    links.reorder(crate::CommandId(1), ProfileId::new(), Vec::new());
    links.reorder(crate::CommandId(2), ProfileId::new(), Vec::new());
    assert!(matches!(
        links.take(),
        Some(Answer::Settled(crate::CommandId(1), _))
    ));
    assert!(matches!(
        links.take(),
        Some(Answer::Settled(crate::CommandId(2), _))
    ));
    assert!(matches!(links.take(), Some(Answer::Unavailable)));
    assert!(links.take().is_none());
}

/// **The four tiers say which they are.** A host that feeds the rows itself
/// must not also be handed a link, and the only way to check that is for the
/// surface to be able to answer.
#[test]
fn a_disabled_link_is_not_held_and_an_inert_one_is() {
    assert!(!WorkspaceLinks::disabled().is_held());
    assert!(
        WorkspaceLinks::inert().is_held(),
        "an inert link stands in for a real one, so it reports as one"
    );
}

/// **A dead worker is one fact, not an endless supply of them.**
///
/// `try_recv` on a closed channel answers `Disconnected` every time it is
/// asked, and the surface drains answers in a loop until there are none — so a
/// link whose worker failed to connect turned the next `poll` into a loop that
/// never ended. It was found by a Koru test *hanging* rather than failing,
/// which is how this class of bug announces itself, and it is pinned here
/// because the loop is the surface's and the silence is the link's.
#[test]
fn a_link_whose_worker_is_gone_says_so_once_and_then_falls_silent() {
    // A link with both ends of its answer channel dropped is exactly the shape
    // a worker that returned leaves behind.
    let links = WorkspaceLinks::open_ended();
    assert!(matches!(
        links.take(),
        Some(crate::state::Answer::Unavailable)
    ));
    assert!(links.take().is_none(), "and it is not said twice");
    assert!(links.take().is_none());

    // The controller's own drain therefore terminates, and says the gateway is
    // gone rather than spinning.
    let mut surface = WorkspacesController::with_links(WorkspaceLinks::open_ended());
    assert!(surface.poll(), "the loss moved something");
    assert_eq!(surface.availability(), crate::Availability::Down);
    assert!(!surface.poll(), "and the second pass has nothing to say");
}

/// **An inert link is quiet, not dead.** A host's `cfg(test)` arrangement must
/// not accidentally arrange the failure state above — a suite that reported
/// every window as having lost its gateway would pass for the wrong reason.
#[test]
fn an_inert_link_answers_nothing_rather_than_reporting_a_loss() {
    let links = WorkspaceLinks::inert();
    assert!(links.take().is_none());
    let mut surface = WorkspacesController::with_links(WorkspaceLinks::inert());
    assert!(!surface.poll());
    assert_eq!(
        surface.availability(),
        crate::Availability::Loading,
        "still looking, not lost"
    );
}

/// A controller with no link asks nothing of anybody, which is what makes it
/// the thing every test constructs.
#[test]
fn a_controller_with_no_link_asks_for_nothing() {
    let surface = WorkspacesController::new();
    assert!(!surface.links().is_held());
    assert_eq!(
        surface.authorization(),
        Authorization::Unknown,
        "nothing was asked, so nothing is known"
    );
}

/// **The scopes a manager names, and why each is named.**
///
/// Naming any scope filters rather than adds, so this list is the whole of what
/// the surface can do. `workspace.destroy` is the one that needs a grant as
/// well; the other three are ordinary and would be lost by omission if the
/// destructive one were named alone.
#[test]
fn the_manager_names_the_destructive_scope_and_the_ordinary_ones_beside_it() {
    assert!(MANAGER_SCOPES.contains(&koru_api_client::WORKSPACE_DESTROY_SCOPE));
    assert!(MANAGER_SCOPES.contains(&"store.manage_workspaces"));
    assert!(
        MANAGER_SCOPES.contains(&koru_api_client::EVENTS_READ_SCOPE),
        "a manager that could not hear another window's change would be the \
         stalest surface in the app"
    );
    assert!(
        !MANAGER_SCOPES.contains(&koru_api_client::CONTROL_SCOPE),
        "managing workspaces is not stopping the store"
    );
    // **And the migration scope beside it, named separately.** A vouched window
    // is granted only what it asks for, so naming the destructive one alone
    // would admit a manager able to remove a workspace and unable to move one
    // — and the row would be dimmed for ever with nothing to say why but the
    // truth, which is that this list forgot to ask (ADR-268).
    assert!(MANAGER_SCOPES.contains(&koru_api_client::WORKSPACE_MIGRATE_SCOPE));
    assert_ne!(
        koru_api_client::WORKSPACE_MIGRATE_SCOPE,
        koru_api_client::WORKSPACE_DESTROY_SCOPE,
        "one scope cannot stand for both"
    );
}

/// **The move's refusals, classified in the worker's language-blind
/// vocabulary.**
///
/// `documents-open` is deliberately unreachable here, and that is the
/// assertion: a move touches no room, so the Store never answers
/// `workspace_in_use` for one (ADR-268). A surface that could produce it would
/// be one telling somebody to close their tabs before moving a workspace —
/// advice about a refusal that does not exist.
#[test]
fn a_moves_refusals_never_include_the_documents_open_one() {
    use koru_api_client::{WorkspaceApiFailure, WorkspaceError};

    let failure = |status: u16, code: &str| {
        WorkspaceError::Api(WorkspaceApiFailure {
            status,
            code: code.to_owned(),
            message: String::new(),
            outcome: None,
            migration: None,
            cause: None,
            retryable: false,
        })
    };
    assert_eq!(
        super::classify_move(&failure(403, "permission_denied")),
        crate::REFUSAL_MOVE_NOT_PERMITTED
    );
    assert_eq!(
        super::classify_move(&failure(404, "not_found")),
        crate::REFUSAL_MOVE_NO_SUCH_PROFILE
    );
    assert_eq!(
        super::classify_move(&failure(503, "server_unavailable")),
        crate::REFUSAL_UNAVAILABLE
    );
    // The Store's own documents-open code, arriving where it cannot: it is not
    // recognised as anything but a refusal nobody can act on.
    assert_eq!(
        super::classify_move(&failure(409, koru_api_client::WORKSPACE_IN_USE)),
        crate::REFUSAL_UNAVAILABLE
    );
}

#[test]
fn a_partial_failure_keeps_its_actionable_documents_open_cause() {
    use koru_api_client::{WorkspaceApiFailure, WorkspaceError};
    use koru_api_wire::{WorkspaceLifecycleAction, WorkspaceLifecycleOutcome};

    let workspace = WorkspaceId::new();
    let error = WorkspaceError::Api(WorkspaceApiFailure {
        status: 409,
        code: "workspace_lifecycle_incomplete".into(),
        message: String::new(),
        outcome: Some(WorkspaceLifecycleOutcome {
            workspace: workspace.to_storage(),
            action: WorkspaceLifecycleAction::SoftDelete,
            terminated_sessions: Vec::new(),
            sessions_closed: true,
            mutation_applied: false,
            authority_committed: None,
            listing: None,
            replacement: None,
        }),
        migration: None,
        cause: Some(koru_api_client::WORKSPACE_IN_USE.into()),
        retryable: true,
    });

    assert_eq!(super::classify(&error), crate::REFUSAL_DOCUMENTS_OPEN);
    let notice = super::refusal(
        workspace,
        WorkspaceLifecycleAction::SoftDelete,
        &error,
        true,
    );
    assert!(notice.retry.is_none());
}

#[test]
fn committed_removal_has_listing_copy_and_the_same_lifecycle_retry() {
    let workspace = WorkspaceId::new();
    let error = koru_api_client::WorkspaceError::Api(koru_api_client::WorkspaceApiFailure {
        status: 409,
        code: "workspace_lifecycle_incomplete".into(),
        message: String::new(),
        outcome: Some(koru_api_wire::WorkspaceLifecycleOutcome {
            workspace: workspace.to_storage(),
            action: koru_api_wire::WorkspaceLifecycleAction::SoftDelete,
            terminated_sessions: vec![],
            sessions_closed: true,
            mutation_applied: false,
            authority_committed: Some(true),
            listing: Some(koru_store_core::service::ListingState::Pending {
                cause: "storage_failed".into(),
            }),
            replacement: Some(koru_store_core::service::Replacement::Unknown),
        }),
        migration: None,
        cause: Some("storage_failed".into()),
        retryable: true,
    });
    let notice = super::refusal(
        workspace,
        koru_api_wire::WorkspaceLifecycleAction::SoftDelete,
        &error,
        true,
    );
    assert_eq!(notice.text, crate::REFUSAL_LISTING_PENDING);
    assert_eq!(
        notice.retry,
        Some(crate::state::Retry::Lifecycle(
            workspace,
            koru_api_wire::WorkspaceLifecycleAction::SoftDelete
        ))
    );
}

/// **A catalog row keeps when it was last opened**, in epoch milliseconds, and
/// a workspace never opened says so with `None` rather than a zero that would
/// read as 1970.
#[test]
fn a_catalog_row_carries_its_last_opened_time() {
    use koru_store_core::model::{Timestamp, Workspace};

    let owner = ProfileId::new();
    let mut opened = Workspace::new(owner, "Notes", Timestamp::from_millis(1_000)).unwrap();
    opened.last_opened_at = Some(Timestamp::from_millis(1_726_000_000_123));
    opened.deleted_at = Some(Timestamp::from_millis(2_000));
    let row = super::row_of(opened.clone());
    assert_eq!(row.id, opened.id);
    assert_eq!(row.owner, owner);
    assert_eq!(row.last_opened_at, Some(1_726_000_000_123));
    assert!(row.deleted);

    let never = Workspace::new(owner, "Fresh", Timestamp::from_millis(1_000)).unwrap();
    assert_eq!(super::row_of(never).last_opened_at, None);
}

/// **A controller on a held link starts knowing nothing.** It has not been
/// vouched for and has not heard a catalog, so it neither offers the
/// destructive rows nor claims the list is empty.
#[test]
fn a_controller_on_a_held_link_starts_unknown_and_loading() {
    let controller = WorkspacesController::with_links(WorkspaceLinks::inert());
    assert!(controller.links().is_held());
    assert_eq!(controller.authorization(), Authorization::Unknown);
    assert_eq!(controller.availability(), crate::Availability::Loading);
}
