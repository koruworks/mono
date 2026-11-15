// SPDX-License-Identifier: MPL-2.0
//! In-memory cards over the real enrollment, replay and tool-row owners.
//! Synthetic delegate observations; not transport or UI qualification.
use super::*;
use koru_delegate_core as d;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_100_000)
}

/// The shared approval row as the AI owner builds it for a Koru decision.
fn approval_row(repo: &Repository, attempt: InferenceAttemptId) -> ToolActivity {
    let (chat, message): (String, String) = repo
        .connection
        .query_row(
            "SELECT chat_id,assistant_message_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: attempt,
        chat_id: ChatId::parse(&chat).unwrap(),
        assistant_message_id: MessageId::parse(&message).unwrap(),
        tool_name: koru_ai_core::AgentToolKind::Approval.as_token().to_owned(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::Pending,
        proposed_at: at(),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}

fn count(repo: &Repository, sql: &str) -> i64 {
    repo.connection.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn decision_card_is_written_once_and_never_duplicated() {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    let first_row = approval_row(&repo, attempt);
    let first = repo
        .record_bot_decision_card(owner, &request.key, &first_row, at())
        .unwrap();
    assert_eq!(first.invocation, first_row.id);
    assert_eq!(first.decision, request.key.decision_id);
    assert_eq!(first.revision, request.key.decision_revision);
    // A retry with another row id replays the one existing card.
    let again = repo
        .record_bot_decision_card(owner, &request.key, &approval_row(&repo, attempt), at())
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(
        count(&repo, "SELECT count(*) FROM bot_decision_invocations"),
        1
    );
    assert_eq!(
        count(
            &repo,
            "SELECT count(*) FROM tool_invocations WHERE tool_name='agent.approval'"
        ),
        1
    );
    let open = repo.tool_activity(first.invocation).unwrap().unwrap();
    assert_eq!(open.status, ToolActivityStatus::ApprovalRequired);
    assert_eq!(open.approval, ToolApproval::Pending);
    assert_eq!(repo.bot_decision_card(first.invocation), Ok(Some(first)));
    assert_eq!(repo.bot_decision_card(ToolInvocationId::new()), Ok(None));
}

#[test]
fn decision_card_needs_the_enrollment_a_live_attempt_and_an_open_approval_row() {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    // No card before the AI holds an authenticated enrollment for the wait.
    assert_eq!(
        repo.record_bot_decision_card(owner, &request.key, &approval_row(&repo, attempt), at()),
        Err(BotError::NotFound)
    );
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    let mut wrong_kind = approval_row(&repo, attempt);
    wrong_kind.tool_name = koru_ai_core::AgentToolKind::Tool.as_token().to_owned();
    assert_eq!(
        repo.record_bot_decision_card(owner, &request.key, &wrong_kind, at()),
        Err(BotError::Invalid)
    );
    let foreign = BotOwner {
        device: DeviceId::new(),
        ..owner
    };
    assert_eq!(
        repo.record_bot_decision_card(foreign, &request.key, &approval_row(&repo, attempt), at()),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE inference_attempts SET status='cancelled',completed_at_ms=queued_at_ms+1 WHERE id=?1",
            [attempt.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.record_bot_decision_card(owner, &request.key, &approval_row(&repo, attempt), at()),
        Err(BotError::Conflict)
    );
    assert_eq!(
        count(&repo, "SELECT count(*) FROM bot_decision_invocations"),
        0
    );
}

#[test]
fn decision_card_lets_the_held_task_roll_over_to_its_successor() {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    let old = repo.bot_replay_state(attempt).unwrap();
    let origin = qualified.cleanup_intent.origin();
    let resume = DecisionResume {
        decision: request.key.decision_id,
        origin,
        consumer: DecisionConsumer::RecordedSuccessor {
            attempt: d::RunAttemptId::mint(),
            generation: d::AttemptGeneration::from_raw(origin.generation.get() + 1).unwrap(),
            creation_key: d::IdempotencyKey::mint(),
            session: Some(d::SessionId::mint()),
        },
        input_key: d::IdempotencyKey::mint(),
        input_fingerprint: [7; 32],
        state: DecisionResumeState::InputAcknowledged {
            acknowledgment: DecisionInputAcknowledgment {
                turn: d::AgentTurnId::mint(),
                receipt_digest: DecisionDigest::from_bytes([8; 32]),
            },
        },
    };
    let fence = old.fence.clone().unwrap();
    // Without the card the existing rollover refuses (O129).
    assert!(
        repo.rollover_bot_decision_replay(
            owner,
            &request.key,
            &resume,
            &fence,
            old.last_sequence,
            at()
        )
        .is_err()
    );
    repo.record_bot_decision_card(owner, &request.key, &approval_row(&repo, attempt), at())
        .unwrap();
    let rolled = repo
        .rollover_bot_decision_replay(
            owner,
            &request.key,
            &resume,
            &fence,
            old.last_sequence,
            at(),
        )
        .unwrap();
    assert_eq!(rolled.old_fence, fence);
    assert_eq!(rolled.installed_after_sequence, None);
}

#[test]
fn native_decision_links_its_prompt_row_once_and_keeps_its_choices() {
    let (mut repo, owner, request, _, attempt) = enrollment::tests::fixture();
    let chat: String = repo
        .connection
        .query_row(
            "SELECT chat_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    let prompt = d::ApprovalId::mint();
    let row = ToolInvocationId::new();
    repo.connection.execute("INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms,bot_source_kind,bot_source_id) VALUES(?1,?2,?3,'agent.approval',0,'approval_required','pending',1,'approval',?4)",
        params![row.to_storage(),attempt.to_storage(),chat,prompt.to_string()]).unwrap();
    let link = BotNativeDecisionLink {
        bot: request.key.bot,
        run: request.key.run,
        decision: DecisionId::mint(),
        revision: DecisionRevision::new(1).unwrap(),
        prompt,
    };
    let card = repo
        .link_bot_native_decision_card(owner, &link, at())
        .unwrap();
    assert_eq!(card.invocation, row);
    assert_eq!(
        repo.link_bot_native_decision_card(owner, &link, at()),
        Ok(card.clone())
    );
    // The same prompt row never becomes the card of a second decision.
    let other = BotNativeDecisionLink {
        decision: DecisionId::mint(),
        ..link.clone()
    };
    assert_eq!(
        repo.link_bot_native_decision_card(owner, &other, at()),
        Err(BotError::Conflict)
    );
    // An unknown prompt is not a card.
    let missing = BotNativeDecisionLink {
        decision: DecisionId::mint(),
        prompt: d::ApprovalId::mint(),
        ..link
    };
    assert_eq!(
        repo.link_bot_native_decision_card(owner, &missing, at()),
        Err(BotError::NotFound)
    );
    assert_eq!(
        count(&repo, "SELECT count(*) FROM bot_decision_invocations"),
        1
    );
    let kept: (String, String) = repo
        .connection
        .query_row(
            "SELECT status,approval FROM tool_invocations WHERE id=?1",
            [row.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kept, ("approval_required".to_owned(), "pending".to_owned()));
}
