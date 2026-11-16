// SPDX-License-Identifier: MPL-2.0
//! The access edit: its receipt, its grant row, its barrier install and its
//! idempotent replay, through the real worker and the real repository.
use super::*;

fn record(repository: &Repository, h: &H, request: BotRequestId) -> BotRoutineAccessRecord {
    repository
        .routine_access_record(h.f.owner, h.bot(), request)
        .unwrap()
        .expect("retained access edit")
}
fn current(repository: &Repository, h: &H) -> BotRoutineCurrentGrant {
    repository
        .routine_current_grant(h.f.owner, h.bot(), h.schedule())
        .unwrap()
}

#[test]
fn routine_access_edit_writes_the_grant_installs_its_barrier_and_completes() {
    let mut h = H::new();
    let request = BotRequestId::mint();
    let edited = h.access(request, 1, BotAccess::Workspace).unwrap();
    assert_eq!(
        edited,
        BotRoutineAccess {
            schedule_id: h.schedule(),
            grant_revision: 2,
            access: BotAccess::Workspace,
            selected_page_count: 0,
        }
    );
    // The barrier for the new revision is installed under the edit's own
    // request, once, and only after the unsettled-work read.
    {
        let installed = h.owner.routine_policies.lock().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].grant_revision, 2);
        assert_eq!(installed[0].request, request.into());
        assert_eq!(installed[0].schedule, h.schedule());
        assert_eq!(
            installed[0].policy.access,
            koru_delegate_core::BotAccess::Workspace
        );
        assert!(installed[0].effects_allowed);
    }
    let calls = h.owner.calls();
    let unsettled = calls.iter().position(|c| c == "unsettled_work").unwrap();
    let install = calls
        .iter()
        .position(|c| c == "synchronize_routine_policy@2")
        .unwrap();
    assert!(unsettled < install, "{calls:?}");
    // The Schedule document carries no grant and was not written.
    assert_eq!(h.owner.live().revision, 1);
    // The detail shows the access now and the access at setup.
    let detail = h.detail();
    assert_eq!(
        (detail.access.grant_revision, detail.access.access),
        (2, BotAccess::Workspace)
    );
    assert_eq!(
        (
            detail.access_at_setup.grant_revision,
            detail.access_at_setup.access
        ),
        (1, BotAccess::Default)
    );

    // A second edit, with a page the person may edit, raises it once more.
    let page = h.f.bot.config.role.clone();
    let second = BotRequestId::mint();
    let BotResponse::RoutineAccess { access } = h
        .send(h.access_command(second, 2, BotAccess::Default, vec![page.clone(), page]))
        .unwrap()
    else {
        panic!("routine_access")
    };
    assert_eq!(
        (
            access.grant_revision,
            access.access,
            access.selected_page_count
        ),
        (3, BotAccess::Default, 1)
    );

    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Completed
    );
    assert_eq!(
        record(&repository, &h, second).stage,
        BotReceiptStage::Completed
    );
    let now = current(&repository, &h);
    assert_eq!((now.grant.grant_revision, now.request), (3, second));
    // The grant it was set up with is never rewritten.
    let creation = repository
        .bot_routine_creation(h.f.owner, h.bot(), h.schedule())
        .unwrap()
        .unwrap();
    assert_eq!(creation.identity.grant, h.f.creation.identity.grant);
    assert_eq!(creation.identity.grant.grant_revision, 1);
}

#[test]
fn routine_access_edit_replays_its_receipt_and_never_raises_the_revision_twice() {
    let mut h = H::new();
    let request = BotRequestId::mint();
    let first = h.access(request, 1, BotAccess::Workspace).unwrap();
    // The same request with the same content: the same answer, no install.
    assert_eq!(h.access(request, 1, BotAccess::Workspace).unwrap(), first);
    assert_eq!(h.owner.routine_policies.lock().unwrap().len(), 1);
    // The same request with other content is a reused request id.
    assert_eq!(
        code(h.access(request, 1, BotAccess::Default)),
        "request_conflict"
    );
    assert_eq!(
        code(h.access(request, 2, BotAccess::Workspace)),
        "request_conflict"
    );
    // A stale grant revision under a new request.
    assert_eq!(
        code(h.access(BotRequestId::mint(), 1, BotAccess::Default)),
        "revision_conflict"
    );
    // After a later edit, the first request still answers what it did.
    let later = h
        .access(BotRequestId::mint(), 2, BotAccess::Default)
        .unwrap();
    assert_eq!(later.grant_revision, 3);
    assert_eq!(h.access(request, 1, BotAccess::Workspace).unwrap(), first);
    assert_eq!(h.owner.routine_policies.lock().unwrap().len(), 2);
    let repository = h.finish();
    assert_eq!(current(&repository, &h).grant.grant_revision, 3);
}

#[test]
fn routine_access_edit_is_refused_while_the_routine_is_on_and_keeps_that_answer() {
    let mut h = H::new();
    h.enable(1).unwrap();
    let request = BotRequestId::mint();
    let refused = h.access(request, 1, BotAccess::Workspace).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (codes::MUST_BE_PAUSED, false)
    );
    assert!(h.owner.routine_policies.lock().unwrap().is_empty());
    // Paused now. The refused request keeps its retained answer; a new one
    // is accepted.
    h.pause(2).unwrap();
    assert_eq!(
        code(h.access(request, 1, BotAccess::Workspace)),
        codes::MUST_BE_PAUSED
    );
    assert_eq!(
        h.access(BotRequestId::mint(), 1, BotAccess::Workspace)
            .unwrap()
            .grant_revision,
        2
    );
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Rejected
    );
    assert_eq!(current(&repository, &h).grant.grant_revision, 2);
}

#[test]
fn routine_access_edit_is_refused_for_a_page_the_person_cannot_edit() {
    let mut h = H::new();
    let page = h.f.bot.config.role.clone();
    *h.authority.forbidden.lock().unwrap() = Some(page.clone());
    let request = BotRequestId::mint();
    let command = h.access_command(request, 1, BotAccess::Default, vec![page.clone()]);
    let refused = h.send(command.clone()).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        ("wrong_owner", false)
    );
    // Retained: the same request gets the same answer even once allowed.
    *h.authority.forbidden.lock().unwrap() = None;
    assert_eq!(code(h.send(command)), "wrong_owner");
    // A page of another workspace is not a page of this Routine at all.
    let foreign = BotPage {
        workspace: WorkspaceId::new(),
        document: page.document,
    };
    assert_eq!(
        code(h.send(h.access_command(BotRequestId::mint(), 1, BotAccess::Default, vec![foreign]))),
        "invalid_request"
    );
    // A page that does not exist is one the person cannot give either: the
    // authority inspection does not tell the two apart.
    let missing = BotPage {
        workspace: page.workspace,
        document: koru_ai_core::DocumentId::new(),
    };
    let gone = BotRequestId::mint();
    assert_eq!(
        code(h.send(h.access_command(gone, 1, BotAccess::Default, vec![missing]))),
        "wrong_owner"
    );
    assert!(h.owner.routine_policies.lock().unwrap().is_empty());
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Rejected
    );
    assert_eq!(
        record(&repository, &h, gone).stage,
        BotReceiptStage::Rejected
    );
    // No refusal wrote a grant.
    assert_eq!(current(&repository, &h).grant.grant_revision, 1);
}

#[test]
fn routine_access_edit_waits_for_unsettled_work_it_cannot_tell_apart() {
    let mut h = H::new();
    let request = BotRequestId::mint();
    // The owner has more queued than one page shows: a Run of this Routine
    // may be among them, so the edit waits. Nothing is retained.
    *h.owner.unsettled.lock().unwrap() = (vec![], true);
    let refused = h.access(request, 1, BotAccess::Workspace).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (codes::RUN_IN_PROGRESS, true)
    );
    assert!(h.owner.routine_policies.lock().unwrap().is_empty());
    // A Run that is not this Routine's (a conversation's, another
    // Routine's) does not hold it, and the same request now succeeds.
    *h.owner.unsettled.lock().unwrap() = (vec![koru_delegate_core::RunId::mint()], false);
    assert_eq!(
        h.access(request, 1, BotAccess::Workspace)
            .unwrap()
            .grant_revision,
        2
    );
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Completed
    );
}

#[test]
fn routine_access_edit_whose_install_is_not_confirmed_stays_pending_until_it_is() {
    let mut h = H::new();
    let request = BotRequestId::mint();
    h.owner.policy_failures.store(usize::MAX, Ordering::SeqCst);
    let refused = h.access(request, 1, BotAccess::Workspace).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (codes::UNAVAILABLE, true)
    );
    // The grant row is written and its receipt is `prepared`: never rolled
    // back, and not yet in force at the owner.
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Prepared
    );
    assert_eq!(current(&repository, &h).grant.grant_revision, 2);
    assert_eq!(
        repository
            .routine_access_pending(h.f.owner, h.bot(), h.schedule())
            .unwrap(),
        Some(request)
    );
    h.restart(repository);
    // While it is pending, the Routine cannot be turned on or tested, and
    // another edit waits.
    let refused = h.enable(1).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["access_pending"]);
    assert_eq!(reasons(&h.test(1).unwrap_err()), ["access_pending"]);
    let waiting = h
        .access(BotRequestId::mint(), 2, BotAccess::Default)
        .unwrap_err();
    assert_eq!(
        (waiting.code.as_str(), waiting.retryable),
        (codes::ACCESS_PENDING, true)
    );
    // Pause is still allowed: it only reduces work.
    assert_eq!(code(h.pause(1)), codes::STATE_CONFLICT);

    // The owner answers again. The same request finishes its own install
    // (or finds the background repeat already did) and never edits twice.
    h.owner.policy_failures.store(0, Ordering::SeqCst);
    let mut answer = None;
    until("the same request completes its install", || {
        match h.access(request, 1, BotAccess::Workspace) {
            Ok(access) => {
                answer = Some(access);
                true
            }
            Err(error) => {
                assert!(
                    error.code == codes::CONTROL_PENDING || error.code == codes::UNAVAILABLE,
                    "{error:?}"
                );
                false
            }
        }
    });
    assert_eq!(answer.unwrap().grant_revision, 2);
    {
        let installed = h.owner.routine_policies.lock().unwrap();
        assert!(!installed.is_empty());
        assert!(
            installed
                .iter()
                .all(|barrier| barrier.grant_revision == 2 && barrier.request == request.into())
        );
    }
    h.enable(1).unwrap();
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Completed
    );
    assert_eq!(current(&repository, &h).grant.grant_revision, 2);
}

#[test]
fn routine_access_restart_repeats_the_install_of_a_prepared_edit_by_itself() {
    let mut h = H::new();
    let request = BotRequestId::mint();
    h.owner.policy_failures.store(usize::MAX, Ordering::SeqCst);
    assert_eq!(
        code(h.access(request, 1, BotAccess::Workspace)),
        codes::UNAVAILABLE
    );
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Prepared
    );
    assert!(h.owner.routine_policies.lock().unwrap().is_empty());
    // A restart, with the owner answering again. Nobody sends the request
    // again: the restart scan finds the prepared receipt and installs it.
    h.owner.policy_failures.store(0, Ordering::SeqCst);
    h.restart(repository);
    until("the restart scan installed the barrier", || {
        !h.owner.routine_policies.lock().unwrap().is_empty()
    });
    until("the Routine can be turned on again", || match h.enable(1) {
        Ok(_) => true,
        Err(error) => {
            assert_eq!(error.code, codes::NOT_READY, "{error:?}");
            false
        }
    });
    let repository = h.finish();
    assert_eq!(
        record(&repository, &h, request).stage,
        BotReceiptStage::Completed
    );
    let installed = h.owner.routine_policies.lock().unwrap();
    assert_eq!(installed[0].grant_revision, 2);
    assert_eq!(installed[0].request, request.into());
}
