// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ProfileId;
use koru_delegate_core as d;

fn fixture(overlap: bool) -> (InferenceAttempt, d::BotSubmission, Vec<PinnedDocument>) {
    let (mut repo, authority, create) = super::super::super::tests::fixture();
    let bot = super::super::super::tests::created(&mut repo, authority, create);
    let page = || BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    };
    let knowledge: Vec<_> = (0..16).map(|_| page()).collect();
    let tasks: Vec<_> = if overlap {
        std::iter::once(bot.config.role.clone())
            .chain(std::iter::once(knowledge[0].clone()))
            .chain((0..14).map(|_| page()))
            .collect()
    } else {
        (0..16).map(|_| page()).collect()
    };
    let sources = super::super::super::library::LibrarySources {
        config_revision: bot.revision,
        policy_revision: bot.policy_revision,
        role: bot.config.role.clone(),
        knowledge,
        task_documents: tasks.clone(),
        memory_binding: None,
        memory_capture: None,
    };
    let captured: Vec<_> = sources
        .pages()
        .iter()
        .map(|page| d::RunContextItem::Document {
            workspace: page.workspace,
            document: page.document,
            version: d::DocumentVersionId::new(),
        })
        .collect();
    let submission = super::super::super::admission::make_library_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &BotTask {
            text: "task".into(),
            documents: tasks.clone(),
        },
        None,
        &captured,
        &sources,
        ProfileId::new(),
    )
    .unwrap()
    .submission;
    let mut attempt = super::super::super::tests::turn(bot.main_chat, Uuid::new_v4(), 1).attempt;
    attempt.context_started_at = Some(Timestamp::from_millis(10));
    attempt.context_completed_at = Some(Timestamp::from_millis(20));
    attempt.context_document_count = 16;
    let pins = tasks
        .into_iter()
        .map(|page| PinnedDocument {
            chat_id: bot.main_chat,
            document_id: page.document,
            display_title: "task page".into(),
            pinned: true,
            first_used_at: Timestamp::from_millis(1),
            last_used_at: Timestamp::from_millis(15),
        })
        .collect();
    (attempt, submission, pins)
}

#[test]
fn resend_keeps_sixteen_tasks_out_of_thirty_three_pins_and_preserves_overlap() {
    for overlap in [false, true] {
        let (attempt, submission, pins) = fixture(overlap);
        assert_eq!(
            submission.request.context.len(),
            if overlap { 31 } else { 33 }
        );
        let refs = resend_task_refs(&attempt, &submission, &pins).unwrap();
        assert_eq!(refs.len(), 16);
        let selected: std::collections::BTreeSet<_> = refs
            .iter()
            .map(|reference| match reference {
                ContextRef::Document { document_id } => *document_id,
                _ => panic!("only documents"),
            })
            .collect();
        assert_eq!(selected, pins.iter().map(|pin| pin.document_id).collect());
        assert_eq!(
            refs,
            resend_task_refs(
                &attempt,
                &submission,
                &pins.iter().cloned().rev().collect::<Vec<_>>()
            )
            .unwrap()
        );
    }
}

#[test]
fn resend_refuses_incomplete_or_mismatched_last_context_proof() {
    for case in 0..8 {
        let (mut attempt, mut submission, mut pins) = fixture(false);
        match case {
            0 => attempt.context_completed_at = None,
            1 => attempt.context_document_count = 15,
            2 => pins[0].last_used_at = Timestamp::from_millis(21),
            3 => pins[0].last_used_at = Timestamp::from_millis(9),
            4 => pins[0].chat_id = ChatId::new(),
            5 => pins[0].pinned = false,
            6 => pins[0].document_id = DocumentId::new(),
            7 => submission.workspace = WorkspaceId::new(),
            _ => unreachable!(),
        }
        assert!(
            resend_task_refs(&attempt, &submission, &pins).is_none(),
            "case {case}"
        );
    }
}
