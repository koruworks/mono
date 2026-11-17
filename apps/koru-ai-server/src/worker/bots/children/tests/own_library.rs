// SPDX-License-Identifier: MPL-2.0
//! Cohort `m6_child_self` (O160): an asked Bot runs as itself. Its Run is
//! captured exactly as a task in its own conversation is, with its own Role,
//! Knowledge and Memory plus the pages the request named, and nothing of the
//! asker's Library; its authority is still the reduced policy of contract
//! 3.3; and every send of one ask carries the same capture.
use super::*;
use std::collections::BTreeSet;

/// The documents a child submission pins as its context.
fn pinned(submission: &d::BotSubmission) -> BTreeSet<koru_ai_core::DocumentId> {
    submission
        .request
        .context
        .iter()
        .filter_map(|pin| match pin {
            d::RunContextItem::Document { document, .. } => Some(*document),
            _ => None,
        })
        .collect()
}

fn attempt_of(submission: &d::BotSubmission) -> InferenceAttemptId {
    InferenceAttemptId::parse(&submission.provenance.ai_attempt_id.unwrap().to_string()).unwrap()
}

/// The stored capture of a child and, when its reply has started, the first
/// input the reply was started with.
fn stored(w: &World, submission: &d::BotSubmission) -> (BotLibraryCapture, Option<String>) {
    let owner = w.authority.owner().unwrap();
    let (bot, request) = (submission.provenance.bot, submission.provenance.submission);
    let attempt = attempt_of(submission);
    on_writer(&w.client, move |state| {
        (
            state
                .repository
                .bot_library_capture(owner, bot, request)
                .unwrap()
                .expect("the child's capture is stored with its receipt"),
            state
                .bot_chats
                .get(&attempt)
                .map(|chat| chat.input_text_for_fixture().to_owned()),
        )
    })
}

fn sent(w: &World) -> Vec<ChildRequest> {
    w.owner.asks.lock().unwrap().clone()
}

#[test]
fn m6_child_self_the_child_runs_with_its_own_role_knowledge_memory_and_the_named_pages() {
    let w = library_world();
    let library = w.library.as_ref().unwrap();
    let helper = w.recipients[0].clone();
    let named = koru_ai_core::DocumentId::new();
    let mut one = item(&helper, "Check the quarter's numbers");
    one.documents = vec![named];
    w.client
        .ask_bots_for_run(ask(&w.subject, Uuid::new_v4(), vec![one]))
        .expect("the ask is admitted");
    let child = sent(&w)[0].requests[0].clone();
    // Its pins: its own Role, its own Knowledge and the named page. Never
    // the asker's Role, and never a Memory page (Memory is not a pin).
    assert_eq!(
        pinned(&child.submission),
        BTreeSet::from([
            helper.config.role.document,
            library.knowledge.document,
            named
        ])
    );
    assert!(!pinned(&child.submission).contains(&w.parent.config.role.document));
    assert_eq!(child.binding.documents, vec![named]);
    // The one Store capture was asked for exactly the pages its own
    // conversation would ask for, Role first.
    assert_eq!(
        w.authority.captured.lock().unwrap().clone(),
        vec![vec![
            helper.config.role.clone(),
            library.knowledge.clone(),
            BotPage {
                workspace: helper.home_workspace,
                document: named
            },
        ]]
    );
    let (capture, input) = stored(&w, &child.submission);
    assert_eq!(capture.bot, helper.id);
    assert_eq!(
        capture
            .knowledge
            .iter()
            .map(|k| k.page.clone())
            .collect::<Vec<_>>(),
        vec![library.knowledge.clone()]
    );
    let BotCapturedMemory::Captured { page, .. } = &capture.memory else {
        panic!("the helper's own Memory is captured");
    };
    assert_eq!(*page, library.helper_memory);
    assert_ne!(*page, library.parent_memory, "never the asker's Memory");
    // Its reply starts with the request as sent, then its own Memory index,
    // as a reply in its own conversation does; nothing of the asker's.
    let input = input.expect("the child's reply has started");
    assert!(input.starts_with(child.submission.input.text.as_str()));
    assert!(input.ends_with(&format!(
        "\n\n{}",
        SelfAuthority::marker(&library.helper_memory)
    )));
    assert!(!input.contains(&SelfAuthority::marker(&library.parent_memory)));
    w.stop();
}

#[test]
fn m6_child_self_the_capture_widens_no_authority() {
    let w = library_world();
    let library = w.library.as_ref().unwrap();
    let helper = w.recipients[0].clone();
    let named = koru_ai_core::DocumentId::new();
    let mut one = item(&helper, "Edit nothing, just read");
    one.documents = vec![named];
    w.client
        .ask_bots_for_run(ask(&w.subject, Uuid::new_v4(), vec![one]))
        .expect("the ask is admitted");
    let child = sent(&w)[0].requests[0].clone();
    // The policy is the one the ask built before captures existed: the
    // recipient's own, its connections reduced to the parent's (3.3).
    let bare = admission::bare_intent(
        &helper,
        BotRequestId::mint(),
        ChatId::new(),
        helper.home_workspace,
        "unused",
        None,
        &[],
        w.profile,
    )
    .unwrap()
    .submission
    .policy;
    let expected =
        koru_ai_core::bots::bot_child_policy(&bare, &w.subject.captured, &w.parent.connections);
    assert_eq!(child.submission.policy, expected);
    assert_eq!(child.submission.policy.access, bare.access);
    assert!(
        child
            .submission
            .policy
            .connections
            .is_subset(&w.subject.captured.connections)
    );
    // No page the capture pinned or read became a page the child may edit.
    for page in [
        helper.config.role.document,
        library.knowledge.document,
        library.helper_memory.document,
        named,
    ] {
        assert!(
            !child.submission.policy.selected_pages.contains(&page),
            "a context page is not a grant"
        );
    }
    assert_eq!(
        child.submission.policy.selected_pages,
        helper
            .selected_pages
            .iter()
            .map(|p| p.document)
            .collect::<BTreeSet<_>>()
    );
    // The binding names exactly that reduced policy.
    let p = &child.submission.provenance;
    assert_eq!(
        child.binding.child_policy,
        ChildPolicyRef::of(
            p.config_revision,
            p.policy_revision,
            &child.submission.policy
        )
        .unwrap()
    );
    w.stop();
}

#[test]
fn m6_child_self_a_resend_reads_back_the_same_capture() {
    let w = library_world();
    let library = w.library.as_ref().unwrap();
    let helper = w.recipients[0].clone();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    owner.answer(Answer::Away);
    let key = Uuid::new_v4();
    let mut short = ask(&w.subject, key, vec![item(&helper, "Help me")]);
    short.deadline = Instant::now() + Duration::from_secs(2);
    assert_eq!(
        code(client.ask_bots_for_run(short)).0,
        codes::CHILD_STATUS_UNKNOWN
    );
    let first = sent(&w)[0].requests[0].clone();
    let (before, _) = stored(&w, &first.submission);
    let captures = w.authority.captured.lock().unwrap().len();
    // The coordination job's resend, as the restart scan sends it: from the
    // stored rows, never a new capture.
    owner.answer(Answer::Create);
    let asked = owner.asks();
    let (bot_id, request) = (helper.id, first.submission.provenance.submission);
    let auth_owner = w.authority.owner().unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while owner.asks() == asked {
        assert!(Instant::now() < until, "the prepared ask is resent");
        on_writer(&client, move |state| {
            let bot = state.repository.bot(bot_id).unwrap().unwrap();
            let Some(BotAdmissionReceipt::Submission(receipt)) = state
                .repository
                .bot_admission_receipt(auth_owner, bot_id, request)
                .unwrap()
            else {
                panic!("a child request is a submission receipt");
            };
            state.queue_child_resend(&bot, &receipt)
        });
        std::thread::sleep(Duration::from_millis(50));
    }
    let resent = sent(&w).last().unwrap().requests[0].clone();
    assert_eq!(
        resent.submission, first.submission,
        "the same pins and input"
    );
    assert_eq!(
        resent.binding.digest().unwrap(),
        first.binding.digest().unwrap()
    );
    assert_eq!(
        w.authority.captured.lock().unwrap().len(),
        captures,
        "a resend captures nothing again"
    );
    // The resend is admitted and its child starts with the same index.
    let until = Instant::now() + Duration::from_secs(10);
    let (after, input) = loop {
        let (after, input) = stored(&w, &first.submission);
        if input.is_some() || Instant::now() >= until {
            break (after, input);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(after, before, "the stored capture is unchanged");
    assert!(input.expect("the resent child starts").ends_with(&format!(
        "\n\n{}",
        SelfAuthority::marker(&library.helper_memory)
    )));
    w.stop();
}

#[test]
fn m6_child_self_the_recipients_memory_page_cannot_be_named() {
    let w = library_world();
    let library = w.library.as_ref().unwrap();
    let mut one = item(&w.recipients[0], "Read your memory page");
    one.documents = vec![library.helper_memory.document];
    let error = w
        .client
        .ask_bots_for_run(ask(&w.subject, Uuid::new_v4(), vec![one]))
        .expect_err("a Memory page is never a context page");
    assert_eq!(error.code, codes::PAGE_UNAVAILABLE);
    assert_eq!(
        error.details.get("document_id"),
        Some(&serde_json::Value::from(
            library.helper_memory.document.to_string()
        ))
    );
    assert_eq!(w.owner.asks(), 0);
    w.stop();
}

#[test]
fn m6_child_self_an_unreadable_memory_index_sends_nothing() {
    let w = library_world();
    let helper = w.recipients[0].clone();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    w.authority
        .unreadable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let key = Uuid::new_v4();
    let items = vec![item(&helper, "Help me")];
    let unknown = client.ask_bots_for_run(ask(&w.subject, key, items.clone()));
    assert_eq!(code(unknown).0, codes::CHILD_STATUS_UNKNOWN);
    assert_eq!(owner.asks(), 0, "never admitted without its Memory");
    let run = w.subject.run;
    let auth_owner = w.authority.owner().unwrap();
    let rows = on_writer(&client, move |state| {
        state
            .repository
            .bot_child_ask(auth_owner, run, key)
            .unwrap()
    });
    assert!(
        rows.iter()
            .all(|row| row.stage == BotReceiptStage::Prepared)
    );
    // Readable again, the same key sends the ask it prepared.
    w.authority
        .unreadable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let admitted = client
        .ask_bots_for_run(ask(&w.subject, key, items))
        .expect("the same key is admitted");
    assert_eq!(admitted.requests[0].1, helper.id);
    assert_eq!(owner.asks(), 1);
    w.stop();
}
