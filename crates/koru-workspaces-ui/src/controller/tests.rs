// SPDX-License-Identifier: MPL-2.0

//! What the controller promises, with nothing drawn.
//!
//! Every controller here holds [`WorkspaceLinks::disabled`], and the answers a
//! link would deliver are fed straight into `receive` instead — the only honest
//! way to test a partial failure, because arranging one for real means closing
//! somebody's terminals and then breaking their Store. Presses are
//! [`Action`]s handed to `perform`, which is exactly what a drawing hands it.

use koru_api_wire::{WorkspaceLifecycleAction, WorkspaceLifecycleOutcome};
use koru_i18n::{Key, Lang, t};
use koru_store_core::ids::{ProfileId, WorkspaceId};
use koru_ui::confirm::Outcome;

use super::*;

#[test]
fn availability_preserves_the_retirement_retry_and_its_original_owner() {
    let mut c = controller();
    c.perform(Action::SoftDelete(id('a')));
    c.receive(Answer::Refused(Notice {
        text: crate::REFUSAL_LISTING_PENDING.into(),
        retry: Some(Retry::Lifecycle(
            id('a'),
            WorkspaceLifecycleAction::SoftDelete,
        )),
    }));
    let (reply, _answer) = std::sync::mpsc::channel();
    c.ensure_live(owner(), "Default".into(), 20, reply);
    assert!(c.notice_for(owner()).unwrap().is_retryable());
    assert!(c.notice_for(elsewhere()).is_none());
    c.perform(Action::Retry);
    assert!(matches!(c.stage, Some(Stage::Asking { confirm: true })));
    assert!(c.notice().is_none());
}

#[test]
fn keep_refusal_is_scoped_to_the_profile_whose_attempt_failed() {
    let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
    let command = c.start_command(Pending::KeepProfile(owner()));
    c.receive(Answer::Settled(
        command,
        Box::new(Answer::ProfileKept(Err(
            koru_api_client::WorkspaceError::Transport(koru_store_core::Error::bare(
                koru_store_core::Code::StorageFailed,
            )),
        ))),
    ));
    assert!(c.notice_for(owner()).is_some());
    assert!(c.notice_for(elsewhere()).is_none());
    assert!(!c.notice_for(owner()).unwrap().is_retryable());
}

#[test]
fn move_retry_notice_keeps_the_source_owner_after_catalog_invalidation() {
    let mut c = controller();
    c.perform(Action::MigrateTo(id('a'), elsewhere()));
    let mut moved = row("Notes", 'a');
    moved.owner = elsewhere();
    c.adopt_rows(&[moved]);
    c.receive(Answer::Refused(Notice {
        text: crate::REFUSAL_MOVE_PARTIAL.into(),
        retry: Some(Retry::Migration(id('a'), elsewhere())),
    }));
    assert!(c.notice_for(owner()).unwrap().is_retryable());
    assert!(c.notice_for(elsewhere()).is_none());
}

#[test]
fn availability_requires_both_command_and_attempt_and_never_lands_itself() {
    let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
    let (reply, answer) = std::sync::mpsc::channel();
    c.ensure_live(owner(), "First".into(), 19, reply);
    let command = *c.pending.keys().next().unwrap();
    c.receive(Answer::Catalog(vec![]));
    assert!(answer.try_recv().is_err());
    assert!(!c.receive(Answer::Settled(
        CommandId(command.0 + 1),
        Box::new(Answer::WorkspaceEnsured {
            attempt: 19,
            result: Ok(koru_api_client::EnsuredWorkspace::Pending { workspaces: vec![] }),
        }),
    )));
    assert!(c.has_pending());
    c.receive(Answer::Settled(
        command,
        Box::new(Answer::WorkspaceEnsured {
            attempt: 19,
            result: Ok(koru_api_client::EnsuredWorkspace::Pending {
                workspaces: vec![id('a')],
            }),
        }),
    ));
    assert!(matches!(answer.try_recv().unwrap().unwrap(),
        koru_api_client::EnsuredWorkspace::Pending { workspaces } if workspaces == vec![id('a')]));
    assert!(!c.has_pending());
    assert!(c.take_requests().is_empty(), "the host owns navigation");
    assert!(!c.receive(Answer::Settled(command, Box::new(Answer::Unavailable))));
}

#[test]
fn mismatched_availability_attempt_and_lost_transport_settle_as_failures() {
    for receipt in [
        Answer::WorkspaceEnsured {
            attempt: 3,
            result: Ok(koru_api_client::EnsuredWorkspace::Pending { workspaces: vec![] }),
        },
        Answer::Unavailable,
    ] {
        let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
        let (reply, answer) = std::sync::mpsc::channel();
        c.ensure_live(owner(), "First".into(), 4, reply);
        let command = *c.pending.keys().next().unwrap();
        c.receive(Answer::Settled(command, Box::new(receipt)));
        assert!(answer.try_recv().unwrap().is_err());
        assert!(!c.has_pending());
        assert!(c.take_requests().is_empty());
    }
}

#[test]
fn abandoned_availability_receiver_does_not_reopen_or_keep_polling() {
    let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
    let (reply, answer) = std::sync::mpsc::channel();
    c.ensure_live(owner(), "First".into(), 4, reply);
    let command = *c.pending.keys().next().unwrap();
    drop(answer);
    c.receive(Answer::Settled(
        command,
        Box::new(Answer::WorkspaceEnsured {
            attempt: 4,
            result: Ok(koru_api_client::EnsuredWorkspace::Pending { workspaces: vec![] }),
        }),
    ));
    assert!(!c.has_pending());
    assert!(c.take_requests().is_empty());
}

#[test]
fn profile_creation_waits_for_its_keyed_answer_and_preserves_typed_refusal() {
    let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
    let (reply, answer) = std::sync::mpsc::channel();
    c.create_profile(
        koru_api_client::CreateProfileWithWorkspace {
            id: ProfileId::new(),
            name: "Owner".into(),
            locale: None,
            purpose: None,
            avatar: None,
            icon: None,
            color: None,
            workspace: koru_api_client::FirstWorkspace {
                name: "First".into(),
                icon: None,
                color: None,
            },
        },
        reply,
    );
    let id = *c.pending.keys().next().unwrap();
    c.receive(Answer::Catalog(vec![]));
    assert!(c.has_pending());
    assert!(answer.try_recv().is_err());
    assert!(!c.receive(Answer::Settled(
        CommandId(id.0 + 1),
        Box::new(Answer::Unavailable)
    )));
    assert!(answer.try_recv().is_err());
    c.receive(Answer::Settled(
        id,
        Box::new(Answer::ProfileCreated(Err(koru_store_core::Error::bare(
            koru_store_core::Code::ProfileExists,
        )))),
    ));
    assert!(!c.has_pending());
    assert_eq!(
        answer.try_recv().unwrap().unwrap_err().code(),
        koru_store_core::Code::ProfileExists
    );
}

#[test]
fn lost_profile_creation_transport_settles_the_waiting_template_worker() {
    let mut c = WorkspacesController::with_links(WorkspaceLinks::inert());
    let (reply, answer) = std::sync::mpsc::channel();
    let id = c.start_command(Pending::CreateProfile(reply));
    c.receive(Answer::Settled(id, Box::new(Answer::Unavailable)));
    assert!(!c.has_pending());
    assert!(answer.try_recv().unwrap().is_err());
}

#[test]
fn first_empty_catalog_and_recovery_are_changes_and_failure_retains_it() {
    let mut controller = WorkspacesController::new();
    assert!(controller.catalog_rows().is_none());
    assert!(controller.adopt_rows(&[]));
    assert_eq!(controller.catalog_rows(), Some([].as_slice()));
    assert!(!controller.adopt_rows(&[]));
    controller.set_availability(Availability::Down);
    assert_eq!(controller.catalog_rows(), Some([].as_slice()));
    assert!(controller.adopt_rows(&[]));
}
use crate::state::{Answer, Consequence, MoveConsequence, ProfileRow, Retry};
use crate::tests::{elsewhere, id, owner, row};

#[test]
fn only_the_matching_settlement_clears_a_pending_operation() {
    let mut c = controller();
    let mut b = row("Other", 'b');
    b.position = 1;
    c.adopt_rows(&[row("Notes", 'a'), b.clone()]);
    c.perform(Action::TogglePinned(id('a')));
    let first = *c.pending.keys().next().unwrap();
    c.perform(Action::TogglePinned(id('a')));
    assert_eq!(c.pending.len(), 1, "one pin per row");
    c.perform(Action::TogglePinned(id('b')));
    c.perform(Action::MoveDown(id('a')));
    assert_eq!(c.pending.len(), 3, "different pins and an order coexist");
    assert!(c.pending_order(owner()).is_some());
    c.perform(Action::MoveDown(id('a')));
    assert_eq!(c.pending.len(), 3, "one order per owner");
    c.receive(Answer::Catalog(vec![row("Renamed elsewhere", 'a'), b]));
    assert_eq!(c.pending_pin(id('a')), Some(true));
    assert_eq!(
        c.pending.len(),
        3,
        "catalog adoption cannot acknowledge a command"
    );
    assert!(!c.receive(Answer::Settled(
        CommandId(999),
        Box::new(Answer::Unavailable)
    )));
    assert_eq!(c.pending.len(), 3);
    assert!(c.receive(Answer::Settled(
        first,
        Box::new(Answer::Refused(Notice {
            text: crate::REFUSAL_UNAVAILABLE.into(),
            retry: None,
        }))
    )));
    assert_eq!(c.pending_pin(id('a')), None);
    assert_eq!(c.pending_pin(id('b')), Some(true));
    assert!(c.pending_order(owner()).is_some());
    assert!(c.notice().is_some());
    assert!(!c.receive(Answer::Settled(
        first,
        Box::new(Answer::Catalog(Vec::new()))
    )));
    assert_eq!(
        c.rows.len(),
        2,
        "a duplicate terminal answer cannot overwrite the catalog"
    );
}

#[test]
fn a_pin_refusal_does_not_cancel_an_unrelated_lifecycle_question() {
    let mut c = controller();
    c.delete_workspace(id('a'));
    c.receive(consequence(
        id('a'),
        WorkspaceLifecycleAction::SoftDelete,
        &[],
    ));
    assert!(c.is_confirming());
    c.perform(Action::TogglePinned(id('a')));
    let command = *c.pending.keys().next().unwrap();
    c.receive(Answer::Settled(
        command,
        Box::new(Answer::Refused(Notice {
            text: crate::REFUSAL_UNAVAILABLE.into(),
            retry: None,
        })),
    ));
    assert!(c.is_confirming());
    assert!(c.notice().is_some());
    assert!(!c.has_pending());
}

// ------------------------------------------------------------------ fixtures

/// A controller with no link and one live workspace selected.
fn controller() -> WorkspacesController {
    let mut controller = WorkspacesController::new();
    controller.adopt_rows(&[row("Notes", 'a')]);
    controller.set_authorization(Authorization::Vouched);
    controller.set_selected(Some(id('a')));
    controller
}

/// The `controller()` fixture, plus the migration permission and a destination.
fn movable() -> WorkspacesController {
    let mut controller = controller();
    controller.set_migration_authorization(Authorization::Vouched);
    controller.adopt_profiles(&[
        ProfileRow {
            id: owner(),
            name: "Work".into(),
        },
        ProfileRow {
            id: elsewhere(),
            name: "Home".into(),
        },
    ]);
    controller
}

fn word(lang: Lang, key: Key) -> Option<String> {
    Some(t(lang, key).to_owned())
}

/// The consequence the gateway would have answered with.
fn consequence(
    workspace: WorkspaceId,
    action: WorkspaceLifecycleAction,
    terminals: &[&str],
) -> Answer {
    Answer::Consequence(Consequence {
        workspace,
        action,
        confirmation: "issued-by-the-gateway".into(),
        terminals: terminals.iter().map(|name| (*name).to_owned()).collect(),
    })
}

fn outcome(
    workspace: WorkspaceId,
    action: WorkspaceLifecycleAction,
    applied: bool,
) -> WorkspaceLifecycleOutcome {
    WorkspaceLifecycleOutcome {
        workspace: workspace.to_storage(),
        action,
        terminated_sessions: Vec::new(),
        sessions_closed: true,
        mutation_applied: applied,
        authority_committed: None,
        listing: None,
        replacement: None,
    }
}

fn moved(projects_rescoped: Option<Vec<String>>) -> Answer {
    Answer::Moved(koru_api_wire::WorkspaceMigrationOutcome {
        workspace: id('a').to_storage(),
        from: owner().to_storage(),
        to: elsewhere().to_storage(),
        terminated_sessions: Vec::new(),
        sessions_closed: true,
        mutation_applied: true,
        projects_rescoped,
        replacement: None,
    })
}

fn owned_by(name: &str, at: char, profile: ProfileId, position: i64) -> Row {
    let mut owned = row(name, at);
    owned.owner = profile;
    owned.position = position;
    owned
}

// ------------------------------------------------------- host-menu intents

/// A host menu can be chosen before a newly opened manager has received
/// either of its asynchronous startup facts. The subject and verb wait here;
/// once catalog and authorization have both arrived, Delete begins the same
/// server-issued consequence flow as the visible detail button.
#[test]
fn a_host_menu_delete_waits_for_catalog_and_authorization() {
    let mut controller = WorkspacesController::new();

    assert!(controller.delete_workspace(id('a')));
    assert!(controller.stage.is_none(), "the catalog is not here yet");
    controller.adopt_rows(&[row("Notes", 'a')]);
    assert!(controller.stage.is_none(), "authorization is not here yet");

    controller.set_authorization(Authorization::Vouched);
    assert!(matches!(
        controller.stage,
        Some(Stage::Asking { confirm: true })
    ));
    assert_eq!(controller.selected(), Some(id('a')));
}

/// The same intent arriving through the link's answers rather than the
/// host's setters, in the other order.
#[test]
fn a_host_menu_intent_waits_for_answers_in_either_order() {
    let mut controller = WorkspacesController::new();
    assert!(controller.archive_workspace(id('a'), true));
    controller.receive(Answer::Authorization(Authorization::Vouched));
    assert!(controller.stage.is_none(), "the catalog is not here yet");
    controller.receive(Answer::Catalog(vec![row("Notes", 'a')]));
    assert!(matches!(
        controller.stage,
        Some(Stage::Asking { confirm: true })
    ));
}

/// An intent whose subject has already left the catalog is dropped, and does
/// not spring back into action if the id is later imported again.
#[test]
fn a_host_menu_intent_for_a_missing_workspace_is_dropped() {
    let mut controller = WorkspacesController::new();
    controller.set_authorization(Authorization::Vouched);
    controller.delete_workspace(id('b'));
    controller.adopt_rows(&[row("Notes", 'a')]);
    assert!(controller.stage.is_none());
    controller.adopt_rows(&[row("Notes", 'a'), row("Back", 'b')]);
    assert!(
        controller.stage.is_none(),
        "the dropped intent stayed dropped"
    );
}

/// A window that may not destroy drops the intent rather than holding it.
#[test]
fn a_host_menu_intent_in_an_unvouched_window_is_dropped() {
    let mut controller = WorkspacesController::new();
    controller.adopt_rows(&[row("Notes", 'a')]);
    controller.delete_workspace(id('a'));
    controller.set_authorization(Authorization::Refused);
    assert!(controller.stage.is_none());
    controller.set_authorization(Authorization::Vouched);
    assert!(controller.stage.is_none(), "nothing was left waiting");
}

// ------------------------------------------------------------------ catalog

/// A selection that has left the list is dropped rather than moved: the detail
/// panel would otherwise describe a workspace that is not there.
#[test]
fn a_selection_that_leaves_the_list_is_dropped() {
    let mut controller = controller();
    assert_eq!(controller.selected(), Some(id('a')));
    controller.adopt_rows(&[row("Elsewhere", 'b')]);
    assert_eq!(controller.selected(), None);
}

/// **One profile's workspaces, grouped and in its own order.** Another
/// profile's rows never appear, and the lifecycle groups follow the same two
/// toggles the list does.
#[test]
fn rows_of_filters_by_owner_and_groups_in_profile_order() {
    let mut controller = WorkspacesController::new();
    let mut pinned = owned_by("Pinned", '1', owner(), 5);
    pinned.pinned = true;
    let mut archived = owned_by("Archived", '2', owner(), 0);
    archived.archived = true;
    let mut trashed = owned_by("Trashed", '3', owner(), 1);
    trashed.deleted = true;
    controller.adopt_rows(&[
        owned_by("Second", '4', owner(), 2),
        owned_by("Theirs", '5', elsewhere(), 0),
        trashed,
        owned_by("First", '6', owner(), 1),
        archived,
        pinned,
    ]);

    let names = |controller: &WorkspacesController, profile| -> Vec<(Group, String)> {
        controller
            .rows_of(profile)
            .into_iter()
            .map(|(group, row)| (group, row.name.clone()))
            .collect()
    };
    assert_eq!(
        names(&controller, owner()),
        vec![
            (Group::Pinned, "Pinned".to_owned()),
            (Group::Active, "First".to_owned()),
            (Group::Active, "Second".to_owned()),
        ],
        "the lifecycle groups are opt-in, and another profile's row is absent"
    );

    controller.set_show_archived(true);
    controller.set_show_trash(true);
    assert_eq!(
        names(&controller, owner()),
        vec![
            (Group::Pinned, "Pinned".to_owned()),
            (Group::Active, "First".to_owned()),
            (Group::Active, "Second".to_owned()),
            (Group::Archived, "Archived".to_owned()),
            (Group::Deleted, "Trashed".to_owned()),
        ]
    );
    assert_eq!(
        names(&controller, elsewhere()),
        vec![(Group::Active, "Theirs".to_owned())]
    );
}

/// The move picker never offers the profile that already owns the workspace.
#[test]
fn destinations_leave_out_the_current_owner() {
    let controller = movable();
    let destinations = controller.destinations(id('a'));
    assert_eq!(
        destinations.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![elsewhere()]
    );
    assert!(controller.destinations(id('f')).is_empty(), "no such row");
}

// ------------------------------------------------------------------ reorder

/// Store accepts only a complete owner permutation. A hidden archived row is
/// retained while the two visible neighbours exchange their actual slots.
#[test]
fn move_down_includes_hidden_owned_workspaces_in_the_store_order() {
    let mut controller = controller();
    let mut first = row("First", 'a');
    first.position = 0;
    let mut hidden = row("Archived", 'b');
    hidden.position = 1;
    hidden.archived = true;
    let mut second = row("Second", 'c');
    second.position = 2;
    controller.adopt_rows(&[first, hidden, second]);

    let (owner, order) = controller
        .reordered_owner_ids(id('a'), 1)
        .expect("First can move below Second");
    assert_eq!(owner, crate::tests::owner());
    assert_eq!(order, vec![id('c'), id('b'), id('a')]);
}

/// A step never crosses into another profile's list, and the first row cannot
/// move up.
#[test]
fn a_reorder_step_stays_inside_its_owner_and_its_ends() {
    let mut controller = WorkspacesController::new();
    controller.adopt_rows(&[
        owned_by("Mine", 'a', owner(), 0),
        owned_by("Theirs", 'b', elsewhere(), 1),
    ]);
    assert!(controller.reordered_owner_ids(id('a'), 1).is_none());
    assert!(controller.reordered_owner_ids(id('a'), -1).is_none());
    assert!(controller.reordered_owner_ids(id('f'), 1).is_none());
    assert!(
        !controller.can_step(id('a'), 1),
        "can_step is the same answer"
    );
}

/// A profile page's *Move Up* / *Move Down* dim at the ends, and its *Show
/// archived (n)* counts a group whether or not it is open.
#[test]
fn can_step_and_count_of_answer_for_one_owner_whatever_is_open() {
    let mut controller = WorkspacesController::new();
    let mut archived = owned_by("Old", 'c', owner(), 2);
    archived.archived = true;
    let mut trashed = owned_by("Gone", 'd', owner(), 3);
    trashed.deleted = true;
    controller.adopt_rows(&[
        owned_by("First", 'a', owner(), 0),
        owned_by("Second", 'b', owner(), 1),
        archived,
        trashed,
        owned_by("Theirs", 'e', elsewhere(), 0),
    ]);
    assert!(!controller.can_step(id('a'), -1));
    assert!(controller.can_step(id('a'), 1));
    assert!(controller.can_step(id('b'), -1));
    assert!(!controller.can_step(id('b'), 1));
    assert!(!controller.show_archived());
    assert_eq!(controller.count_of(owner(), Group::Active), 2);
    assert_eq!(controller.count_of(owner(), Group::Archived), 1);
    assert_eq!(controller.count_of(owner(), Group::Deleted), 1);
    assert_eq!(controller.count_of(elsewhere(), Group::Active), 1);
}

// ------------------------------------------------------------- S13, the confirmation

/// **S13.** The consequence names the terminals the *gateway* computed — the
/// dialog is not built until the answer arrives, so there is no moment at
/// which it is showing a guess.
#[test]
fn s13_the_confirmation_names_the_terminals_the_gateway_said_would_close() {
    let mut controller = controller();
    controller.perform(Action::Archive(id('a'), true));
    assert!(
        !controller.is_confirming(),
        "nothing is drawn before the answer"
    );
    assert!(controller.dialog().is_none());

    controller.receive(consequence(
        id('a'),
        WorkspaceLifecycleAction::Archive,
        &["zsh — my-project", "cargo watch"],
    ));
    assert!(controller.is_confirming());
    assert!(controller.dialog().is_some());
    assert_eq!(
        controller.confirmation_terminals(),
        ["zsh — my-project", "cargo watch"]
    );
}

/// **S13 / ADR-265.** Archive and soft-delete are reversible confirmations the
/// person explicitly invoked, so both **declare their primary** and Enter
/// accepts.
#[test]
fn s13_archive_and_soft_delete_declare_their_primary_action() {
    for (press, action) in [
        (
            Action::Archive(id('a'), true),
            WorkspaceLifecycleAction::Archive,
        ),
        (
            Action::SoftDelete(id('a')),
            WorkspaceLifecycleAction::SoftDelete,
        ),
    ] {
        let mut controller = controller();
        controller.perform(press);
        controller.receive(consequence(id('a'), action, &["zsh"]));
        assert_eq!(
            controller.confirmation_default(),
            Some(Outcome::Confirm),
            "{action:?} should declare its primary (ADR-265 row two)"
        );
    }
}

/// A workspace with nothing open in it says so, rather than showing an empty
/// list and leaving somebody to work out what that means.
#[test]
fn s13_a_workspace_with_no_terminals_says_so() {
    let mut controller = controller();
    controller.perform(Action::Archive(id('a'), true));
    controller.receive(consequence(id('a'), WorkspaceLifecycleAction::Archive, &[]));
    assert!(controller.confirmation_terminals().is_empty());
    let body = controller.dialog().expect("the dialog is up").body();
    assert!(body.contains(t(Lang::En, Key::WorkspacesNoTerminals)));
}

/// Cancelling leaves the token unspent and reports nothing; confirming moves
/// the flow on to the operation.
#[test]
fn answering_the_confirmation_cancels_or_proceeds() {
    let mut controller = controller();
    controller.perform(Action::SoftDelete(id('a')));
    controller.receive(consequence(
        id('a'),
        WorkspaceLifecycleAction::SoftDelete,
        &[],
    ));
    assert!(controller.answer(Outcome::Cancel));
    assert!(!controller.is_confirming());
    assert!(controller.notice().is_none());

    controller.perform(Action::SoftDelete(id('a')));
    controller.receive(consequence(
        id('a'),
        WorkspaceLifecycleAction::SoftDelete,
        &[],
    ));
    assert!(controller.answer(Outcome::Confirm));
    assert!(matches!(controller.stage, Some(Stage::Working)));
}

/// Restoring closes nothing, so it asks for its token without a question.
#[test]
fn restore_spends_its_token_without_a_dialog() {
    let mut controller = controller();
    controller.perform(Action::Restore(id('a')));
    assert!(matches!(
        controller.stage,
        Some(Stage::Asking { confirm: false })
    ));
    controller.receive(consequence(id('a'), WorkspaceLifecycleAction::Restore, &[]));
    assert!(!controller.is_confirming());
    assert!(matches!(controller.stage, Some(Stage::Working)));
}

/// A consequence nobody is waiting for opens nothing.
#[test]
fn a_consequence_nobody_asked_for_opens_nothing() {
    let mut controller = controller();
    assert!(!controller.receive(consequence(id('a'), WorkspaceLifecycleAction::Archive, &[])));
    assert!(!controller.is_confirming());
}

/// The consequence sentence is built from keys in every language, including the
/// one that carries a count — a dialog that read *{} terminals* to somebody
/// would be the failure this test exists for.
#[test]
fn the_consequence_sentence_is_localized_including_its_count() {
    for lang in Lang::ALL {
        let mut controller = controller();
        controller.set_lang(lang);
        controller.perform(Action::SoftDelete(id('a')));
        controller.receive(consequence(
            id('a'),
            WorkspaceLifecycleAction::SoftDelete,
            &["zsh", "cargo"],
        ));
        let dialog = controller
            .dialog()
            .unwrap_or_else(|| panic!("{lang:?} did not raise the confirmation"));
        assert!(
            !dialog.title().contains("{}"),
            "{lang:?} left a placeholder"
        );
        assert!(!dialog.body().contains("{}"), "{lang:?} left a placeholder");
        assert!(
            dialog.body().contains("zsh"),
            "{lang:?} dropped the terminals from the consequence"
        );
    }
}

// ---------------------------------------------------------------- S17 and S18

/// Archiving follows the workspace to where it went. A row that simply
/// vanished would leave somebody looking for it.
#[test]
fn s17_completing_an_archive_opens_the_group_the_workspace_moved_to() {
    let mut archiving = controller();
    assert!(!archiving.show_archived());
    archiving.receive(Answer::Completed(outcome(
        id('a'),
        WorkspaceLifecycleAction::Archive,
        true,
    )));
    assert!(archiving.show_archived());

    let mut deleting = controller();
    deleting.receive(Answer::Completed(outcome(
        id('a'),
        WorkspaceLifecycleAction::SoftDelete,
        true,
    )));
    assert!(deleting.show_trash());
}

// ------------------------------------------------------------------------ S24

/// **S24, presented as exactly the truth it is**, with a retry.
#[test]
fn s24_a_partial_failure_says_what_happened_and_offers_a_retry() {
    let mut controller = controller();
    controller.receive(Answer::Refused(Notice {
        text: REFUSAL_PARTIAL.into(),
        retry: Some(Retry::Lifecycle(
            id('a'),
            WorkspaceLifecycleAction::SoftDelete,
        )),
    }));
    let notice = controller.notice().expect("S24 leaves a notice");
    assert_eq!(notice.text(), t(Lang::En, Key::WorkspacesPartialFailure));
    assert!(notice.is_retryable());
}

/// **The retry asks for a fresh consequence.** The first token was spent, and
/// what it named is a workspace whose terminals are already gone.
#[test]
fn s24_the_retry_asks_again_rather_than_reusing_the_spent_token() {
    let mut controller = controller();
    controller.receive(Answer::Refused(Notice {
        text: REFUSAL_PARTIAL.into(),
        retry: Some(Retry::Lifecycle(
            id('a'),
            WorkspaceLifecycleAction::SoftDelete,
        )),
    }));
    controller.perform(Action::Retry);
    assert!(controller.notice().is_none(), "the notice is consumed");
    assert!(
        !controller.is_confirming(),
        "and nothing is on screen until the new consequence arrives"
    );
    assert!(matches!(
        controller.stage,
        Some(Stage::Asking { confirm: true })
    ));

    controller.receive(consequence(
        id('a'),
        WorkspaceLifecycleAction::SoftDelete,
        &[],
    ));
    assert!(controller.is_confirming());
    assert!(controller.confirmation_terminals().is_empty());
}

/// The documents-connected refusal, said as what it is, for an older gateway.
#[test]
fn an_older_gateways_documents_open_refusal_remains_intelligible() {
    let mut controller = controller();
    controller.receive(Answer::Refused(Notice {
        text: REFUSAL_DOCUMENTS_OPEN.into(),
        retry: None,
    }));
    assert_eq!(
        controller.notice().map(Notice::text),
        Some(t(controller.lang(), Key::WorkspacesDocumentsOpen))
    );
    assert!(!controller.notice().unwrap().is_retryable());
}

/// **S9.** A gateway that stops answering ends whatever was mid-flow.
#[test]
fn s9_losing_the_gateway_ends_the_flow_and_says_so_once() {
    let mut controller = controller();
    controller.perform(Action::Archive(id('a'), true));
    assert!(controller.receive(Answer::Unavailable));
    assert!(controller.availability().is_down());
    assert!(controller.stage.is_none());
    assert!(!controller.receive(Answer::Unavailable), "said once");
}

// ---------------------------------------- moving to another profile (SP-15a)

/// Opening the picker asks the gateway nothing; choosing a destination does.
#[test]
fn opening_the_picker_is_not_the_operation() {
    let mut controller = movable();
    controller.perform(Action::Migrate(id('a')));
    assert!(controller.is_choosing_profile());
    assert_eq!(controller.choosing(), Some(id('a')));
    assert!(!controller.is_confirming());

    controller.perform(Action::CancelMigrate);
    assert!(!controller.is_choosing_profile());
    assert!(controller.stage.is_none());
}

/// **The move dialog is built from the gateway's answer and declares its
/// primary** (ADR-265 row two).
#[test]
fn the_move_confirmation_names_both_ends_and_declares_its_primary() {
    let mut controller = movable();
    controller.perform(Action::Migrate(id('a')));
    controller.perform(Action::MigrateTo(id('a'), elsewhere()));
    assert!(matches!(controller.stage, Some(Stage::AskingMove)));
    assert!(!controller.is_confirming());

    controller.receive(Answer::MoveConsequence(MoveConsequence {
        workspace: id('a'),
        to: elsewhere(),
        to_name: "Home".into(),
        confirmation: "token".into(),
        terminals: vec!["build".into(), "logs".into()],
    }));
    assert!(controller.is_confirming());
    assert_eq!(controller.confirmation_terminals(), ["build", "logs"]);
    assert_eq!(controller.confirmation_default(), Some(Outcome::Confirm));
    let title = controller.dialog().expect("the move dialog").title();
    assert!(title.contains("Notes") && title.contains("Home"), "{title}");
}

/// **A late answer nobody is waiting for is dropped.**
#[test]
fn a_move_consequence_nobody_asked_for_opens_nothing() {
    let mut controller = movable();
    controller.receive(Answer::MoveConsequence(MoveConsequence {
        workspace: id('a'),
        to: elsewhere(),
        to_name: "Home".into(),
        confirmation: "token".into(),
        terminals: Vec::new(),
    }));
    assert!(!controller.is_confirming());
}

/// A partial move offers a retry that asks for a fresh move consequence.
#[test]
fn a_partial_moves_retry_asks_again() {
    let mut controller = movable();
    controller.receive(Answer::Refused(Notice {
        text: REFUSAL_MOVE_PARTIAL.into(),
        retry: Some(Retry::Migration(id('a'), elsewhere())),
    }));
    let notice = controller.notice().expect("a partial move leaves a notice");
    assert_eq!(
        notice.text(),
        t(Lang::En, Key::WorkspacesMovePartialFailure)
    );
    controller.perform(Action::Retry);
    assert!(controller.notice().is_none());
    assert!(matches!(controller.stage, Some(Stage::AskingMove)));
}

/// **A move that committed while the session host was silent is not a
/// failure**, and it does not pretend to be a clean success either.
#[test]
fn a_move_whose_projects_did_not_follow_says_so_and_offers_to_finish_it() {
    let mut controller = movable();
    controller.receive(moved(None));
    let notice = controller
        .notice()
        .expect("an unrepaired move leaves a notice");
    assert_eq!(
        notice.text(),
        t(Lang::En, Key::WorkspacesMoveProjectsPending)
    );
    assert!(notice.is_retryable());
}

/// And a move whose projects did follow says nothing at all.
#[test]
fn a_move_whose_projects_followed_leaves_no_notice() {
    let mut controller = movable();
    controller.receive(moved(Some(Vec::new())));
    assert!(controller.notice().is_none());
    assert!(!controller.is_confirming());
}

// ------------------------------------------------------------------ reasons

/// **Every reason, in the order it outranks the others.** The sentence a
/// drawing puts beside a dimmed control, asked of the controller directly.
#[test]
fn reason_for_ordinary_actions_follows_gateway_then_trash() {
    let lang = Lang::En;
    let mut controller = controller();
    let live = row("Notes", 'a');
    for action in [
        Action::Open(id('a')),
        Action::OpenInNewWindow(id('a')),
        Action::Rename(id('a')),
        Action::Appearance(id('a')),
        Action::TogglePinned(id('a')),
        Action::MoveUp(id('a')),
        Action::MoveDown(id('a')),
        Action::Export(id('a')),
        Action::CopyId(id('a')),
        Action::Create,
        Action::Import,
    ] {
        assert_eq!(controller.reason_for(&action, &live), None, "{action:?}");
    }

    let mut trashed = live.clone();
    trashed.deleted = true;
    assert_eq!(
        controller.reason_for(&Action::Open(id('a')), &trashed),
        word(lang, Key::WorkspacesTrash)
    );
    assert_eq!(
        controller.reason_for(&Action::Export(id('a')), &trashed),
        None,
        "a trashed workspace can still be exported"
    );

    controller.set_availability(Availability::Down);
    for action in [
        Action::Open(id('a')),
        Action::Export(id('a')),
        Action::Create,
        Action::Import,
    ] {
        assert_eq!(
            controller.reason_for(&action, &trashed),
            word(lang, Key::WorkspacesUnavailableReadOnly),
            "{action:?}: the gateway being down outranks the trash"
        );
    }
    assert_eq!(
        controller.reason_for(&Action::CopyId(id('a')), &trashed),
        None,
        "the clipboard needs neither a gateway nor a permission"
    );
}

/// Archive, Delete and Restore need `workspace.destroy`; Archive also refuses
/// a trashed workspace; a gateway that is down outranks both.
#[test]
fn reason_for_destructive_actions_follows_gateway_then_permission() {
    let lang = Lang::En;
    let mut controller = controller();
    let live = row("Notes", 'a');
    let mut trashed = live.clone();
    trashed.deleted = true;

    assert_eq!(
        controller.reason_for(&Action::Archive(id('a'), true), &live),
        None
    );
    assert_eq!(
        controller.reason_for(&Action::Archive(id('a'), true), &trashed),
        word(lang, Key::WorkspacesTrash)
    );
    assert_eq!(
        controller.reason_for(&Action::Restore(id('a')), &trashed),
        None
    );

    controller.set_authorization(Authorization::Refused);
    for action in [
        Action::Archive(id('a'), true),
        Action::SoftDelete(id('a')),
        Action::Restore(id('a')),
    ] {
        assert_eq!(
            controller.reason_for(&action, &trashed),
            word(lang, Key::WorkspacesNotPermitted),
            "{action:?}"
        );
    }
    assert_eq!(
        controller.reason_for(&Action::Open(id('a')), &live),
        None,
        "a narrow permission, not a read-only manager"
    );

    controller.set_availability(Availability::Down);
    assert_eq!(
        controller.reason_for(&Action::SoftDelete(id('a')), &live),
        word(lang, Key::WorkspacesUnavailableReadOnly)
    );
}

/// The move row's four reasons, each outranking the next.
#[test]
fn reason_for_the_move_follows_gateway_permission_trash_destinations() {
    let lang = Lang::En;
    let migrate = Action::Migrate(id('a'));
    let live = row("Notes", 'a');
    let mut trashed = live.clone();
    trashed.deleted = true;

    let mut controller = movable();
    assert_eq!(controller.reason_for(&migrate, &live), None);
    assert_eq!(
        controller.reason_for(&migrate, &trashed),
        word(lang, Key::WorkspacesTrash)
    );

    controller.adopt_profiles(&[ProfileRow {
        id: owner(),
        name: "Work".into(),
    }]);
    assert_eq!(
        controller.reason_for(&migrate, &live),
        word(lang, Key::WorkspacesMoveNoProfiles)
    );
    assert_eq!(
        controller.reason_for(&migrate, &trashed),
        word(lang, Key::WorkspacesTrash),
        "the trash outranks having nowhere to go"
    );

    controller.set_migration_authorization(Authorization::Refused);
    assert_eq!(
        controller.reason_for(&migrate, &trashed),
        word(lang, Key::WorkspacesMoveNotPermitted)
    );

    controller.set_availability(Availability::Down);
    assert_eq!(
        controller.reason_for(&migrate, &trashed),
        word(lang, Key::WorkspacesUnavailableReadOnly)
    );
}

/// The controller's own controls never carry a reason.
#[test]
fn reason_for_the_controllers_own_controls_is_none() {
    let mut controller = controller();
    controller.set_availability(Availability::Down);
    controller.set_authorization(Authorization::Refused);
    let live = row("Notes", 'a');
    for action in [
        Action::Select(id('a')),
        Action::MigrateTo(id('a'), elsewhere()),
        Action::CancelMigrate,
        Action::ShowArchived,
        Action::ShowTrash,
        Action::Retry,
        Action::DismissNotice,
    ] {
        assert_eq!(controller.reason_for(&action, &live), None, "{action:?}");
    }
}

// ------------------------------------------------------------------ requests

#[test]
fn removal_marks_survive_transport_loss_and_block_previously_drawn_navigation() {
    let mut controller = controller();
    let mark = koru_store_core::profile_removal::RemovalMark {
        attempt: koru_store_core::ids::RemovalAttemptId::new(),
        kind: koru_store_core::profile_removal::RemovalKind::Local,
        started_at: koru_store_core::Timestamp::from_millis(1),
        last_step_at: koru_store_core::Timestamp::from_millis(1),
        stopped: true,
    };
    assert!(controller.profile_removal_state(owner()).is_none());
    controller.receive(Answer::CatalogState {
        rows: vec![row("Notes", 'a')],
        removals: [(owner(), mark.clone())].into(),
    });
    assert_eq!(
        controller.profile_removal_state(owner()),
        Some(Some(mark.clone()))
    );
    controller.perform(Action::Open(id('a')));
    assert!(controller.take_requests().is_empty());
    assert!(
        controller
            .reason_for(&Action::Open(id('a')), controller.row(id('a')).unwrap())
            .is_some()
    );
    controller.receive(Answer::Unavailable);
    assert_eq!(controller.profile_removal_state(owner()), Some(Some(mark)));
    controller.receive(Answer::CatalogState {
        rows: vec![row("Notes", 'a')],
        removals: Default::default(),
    });
    assert_eq!(controller.profile_removal_state(owner()), Some(None));
    controller.perform(Action::Open(id('a')));
    assert_eq!(controller.take_requests(), vec![Request::Open(id('a'))]);
}

#[test]
fn stale_navigation_actions_recheck_authority_and_connection() {
    for new_window in [false, true] {
        let action = if new_window {
            Action::OpenInNewWindow(id('a'))
        } else {
            Action::Open(id('a'))
        };
        for state in 0..7 {
            let mut controller = controller();
            let mut changed = row("Notes", 'a');
            match state {
                0 => changed.authority = crate::Authority::Unknown,
                1 => changed.authority = crate::Authority::Pending,
                2 => {
                    changed.authority = crate::Authority::Uncertain {
                        cause: "storage_failed".into(),
                        recovery: None,
                    }
                }
                3 => changed.deleted = true,
                4 => changed.archived = true,
                5 => changed.listing_differs = true,
                _ => {}
            }
            controller.adopt_rows(&[changed]);
            if state == 6 {
                controller.set_availability(Availability::Down);
            }
            controller.perform(action.clone());
            assert!(controller.take_requests().is_empty(), "state {state}");
        }
    }
}

/// **Everything that needs a window is a request; everything else is not.**
#[test]
fn only_the_things_that_need_a_window_become_requests() {
    let mut controller = controller();
    for (action, request) in [
        (Action::Open(id('a')), Request::Open(id('a'))),
        (
            Action::OpenInNewWindow(id('a')),
            Request::OpenInNewWindow(id('a')),
        ),
        (Action::Create, Request::Create),
        (Action::Rename(id('a')), Request::Rename(id('a'))),
        (
            Action::Appearance(id('a')),
            Request::ChangeAppearance(id('a')),
        ),
        (Action::Import, Request::Import),
        (Action::Export(id('a')), Request::Export(id('a'))),
        (Action::CopyId(id('a')), Request::CopyId(id('a'))),
    ] {
        controller.perform(action);
        assert_eq!(controller.take_requests(), vec![request]);
    }
    for action in [
        Action::TogglePinned(id('a')),
        Action::Archive(id('a'), true),
        Action::SoftDelete(id('a')),
        Action::ShowArchived,
        Action::Select(id('a')),
    ] {
        controller.perform(action);
        assert!(controller.take_requests().is_empty(), "{action:?}");
    }
    assert!(controller.rename_selected());
    assert_eq!(controller.take_requests(), vec![Request::Rename(id('a'))]);
}
