// SPDX-License-Identifier: MPL-2.0

//! Opt-in current-route authority, rejection and worker-recovery evidence.
//! All accounts remain runtime-owned; every Koru write uses a disposable root.
use super::*;
use koru_session_protocol::{
    ApprovalChoiceId, ApprovalDecision, ApprovalId, ApprovalIntent, ControllerLeaseToken,
    ErrorCode, HandoverState, IdempotencyKey, TakeoverUrgency, ViewportGeneration,
};

const REFUSED_FILE: &str = "approval-refused.txt";
const REFUSED_OBJECTIVE: &str = "This is a permission rejection test. Request normal approval through your native command tool before running a single PowerShell command: Set-Content -LiteralPath 'approval-refused.txt' -Value 'KORU_DECLINED_FIXTURE'. The file would be in the current working directory. Ask with the command tool's sandbox_permissions=require_escalated; do not change approval mode. The person will reject it. After rejection, do not retry, use another tool or create anything; report the refusal and finish. No network, other files, MCP tools or child agents.";

fn with_control<T>(
    sessions: &SessionClient,
    session: SessionId,
    operation: impl FnOnce(ControllerLeaseToken) -> T,
) -> T {
    sessions
        .request_control(session, TakeoverUrgency::AtTurnBoundary)
        .unwrap();
    wait_until("probe approval control handover", PROMPT, || {
        sessions
            .list()
            .unwrap()
            .iter()
            .find(|row| row.id == session)
            .is_some_and(|row| row.handover == HandoverState::Released)
    });
    let (lease, _subscription) = sessions
        .attach_live(session, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let result = operation(lease.token);
    sessions
        .return_control(session, lease.token)
        .expect("probe returns control");
    result
}

fn cancel_probe(fixture: &LiveFixture, run: RunId) {
    fixture.client.runs().cancel(run).expect("cancel probe");
    wait_until("probe cancellation", PROMPT, || {
        fixture
            .client
            .runs()
            .run(run, None, 0, 500)
            .is_ok_and(|view| view.header.status == RunStatus::Cancelled)
    });
    verify_closed(&fixture.agent, &fixture.client, &fixture.sessions, run);
}

/// No interpretation of tool prose is needed to deny permission. Never grants
/// an Allow or remembered option, and refuses ambiguous one-time rejections.
fn pending_rejection(
    fixture: &LiveFixture,
    run: RunId,
) -> (SessionId, ApprovalId, ApprovalChoiceId) {
    let mut result = None;
    wait_until("native runtime permission request", LIVE_DEADLINE, || {
        assert!(!fixture.directory.join(REFUSED_FILE).exists());
        let Some(session) = session_for(&fixture.agent, run) else {
            return false;
        };
        let observation = fixture.sessions.observe_agent_events(session).unwrap();
        assert!(!observation.earlier_truncated);
        let mut pending = std::collections::BTreeMap::new();
        for item in &observation.events {
            match &item.event {
                AgentEvent::ApprovalRequired {
                    approval_id,
                    choices,
                    summary,
                    ..
                } => {
                    assert!(
                        summary == "command_execution",
                        "different native approval category; command rejection remains unverified"
                    );
                    pending.insert(*approval_id, choices);
                }
                AgentEvent::ApprovalResolved { approval_id, .. }
                | AgentEvent::ApprovalExpired { approval_id, .. } => {
                    pending.remove(approval_id);
                }
                _ => {}
            }
        }
        assert!(
            pending.len() <= 1,
            "ambiguous permission requests; grant nothing"
        );
        let Some((&approval, choices)) = pending.first_key_value() else {
            let view = fixture.client.runs().run(run, None, 0, 500).unwrap();
            assert!(
                !view.header.status.is_terminal()
                    && !matches!(&view.header.status, RunStatus::Running { attention, .. }
                        if attention.contains(Attention::FinishedNeedingAttention)),
                "runtime ended without a native approval; approval row remains unverified"
            );
            return false;
        };
        let rejects: Vec<_> = choices
            .iter()
            .filter(|choice| choice.intent == ApprovalIntent::Reject)
            .collect();
        assert_eq!(rejects.len(), 1, "one unambiguous one-time rejection");
        result = Some((session, approval, rejects[0].id));
        true
    });
    result.unwrap()
}

#[test]
fn live_codex_rejects_write_and_refuses_stale_answer_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let mut fixture = live_fixture(&CODEX);
    let run = fixture.request.run;
    fixture.request.objective = Objective::new(REFUSED_OBJECTIVE).unwrap();
    assert!(!fixture.directory.join(REFUSED_FILE).exists());
    fixture.client.runs().create(&fixture.request).unwrap();
    let (session, approval, reject) = pending_rejection(&fixture, run);
    with_control(&fixture.sessions, session, |lease| {
        fixture
            .sessions
            .answer_approval(
                session,
                lease,
                IdempotencyKey::mint(),
                approval,
                ApprovalDecision::Reject,
                Some(reject),
            )
            .expect("one-time rejection accepted");
        // A new command key tests stale prompt identity, not replay deduplication.
        let stale = fixture.sessions.answer_approval(
            session,
            lease,
            IdempotencyKey::mint(),
            approval,
            ApprovalDecision::Reject,
            Some(reject),
        );
        assert!(
            matches!(stale,
                Err(koru_session_client::ClientError::Session(error))
                    if matches!(error.code, ErrorCode::ApprovalNotFound | ErrorCode::ApprovalStale)
            ),
            "old approval must be refused by identity, not a lost controller"
        );
    });
    wait_until("rejected write ends without output", LIVE_DEADLINE, || {
        assert!(!fixture.directory.join(REFUSED_FILE).exists());
        let view = fixture.client.runs().run(run, None, 0, 500).unwrap();
        assert!(view.header.artifacts.is_empty());
        matches!(view.header.status, RunStatus::Running { attention, .. }
            if attention.contains(Attention::FinishedNeedingAttention))
    });
    assert!(
        fixture
            .client
            .artifacts()
            .run_versions(run)
            .unwrap()
            .versions
            .is_empty()
    );
    cancel_probe(&fixture, run);
    assert!(!fixture.directory.join(REFUSED_FILE).exists());
    eprintln!(
        "live_approval provider=codex version={} category=command_execution reject=confirmed stale=refused target_file_absent=true artifacts=0 cleanup=confirmed",
        fixture.version
    );
}

fn active_text_session(fixture: &LiveFixture, run: RunId) -> SessionId {
    let mut result = None;
    wait_until(
        "active harmless text before interruption",
        LIVE_DEADLINE,
        || {
            let Some(session) = session_for(&fixture.agent, run) else {
                return false;
            };
            let observation = fixture.sessions.observe_agent_events(session).unwrap();
            assert!(!observation.earlier_truncated);
            let summary = fixture
                .sessions
                .list()
                .unwrap()
                .into_iter()
                .find(|row| row.id == session);
            let Some(summary) = summary else { return false };
            let active = summary.active_turn_id;
            if summary.managed == Some(koru_session_protocol::ManagedState::Running)
            && active.is_some()
            && observation.events.iter().any(|item| matches!(&item.event,
                AgentEvent::MessageDelta { turn_id, text } if Some(*turn_id) == active && !text.is_empty()))
        {
            assert!(summary.origin.chat().is_none());
            result = Some(session);
            true
        } else { false }
        },
    );
    result.unwrap()
}

#[test]
fn live_codex_recovers_worker_loss_without_an_implicit_attempt_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let mut fixture = live_fixture(&CODEX);
    let run = fixture.request.run;
    fixture.request.objective = Objective::new(CANCEL_OBJECTIVE).unwrap();
    fixture.client.runs().create(&fixture.request).unwrap();
    let session = active_text_session(&fixture, run);
    let before = fixture.agent.delegate().hello().unwrap();
    let first = fixture
        .agent
        .runs_db()
        .current_attempt(run)
        .unwrap()
        .unwrap();
    kill_pid(before.pid);
    // Exercise owner loss plus provider loss through the existing scoped
    // lifecycle hook. This is not a machine power-loss or native-resume claim.
    fixture
        .sessions
        .terminate_delegated(session)
        .expect("end only this delegated provider");
    wait_until("owner restarts", Duration::from_secs(90), || {
        fixture
            .agent
            .delegate()
            .hello()
            .is_ok_and(|hello| hello.pid != before.pid && hello.launch_id != before.launch_id)
    });
    wait_until("interruption reconciles", Duration::from_secs(90), || {
        let view = fixture.client.runs().run(run, None, 0, 500).unwrap();
        assert!(!matches!(view.header.status, RunStatus::Failed { .. }));
        matches!(view.header.status, RunStatus::Interrupted { .. })
    });
    assert_eq!(fixture.agent.runs_db().attempts(run).unwrap().len(), 1);
    assert!(
        fixture
            .client
            .artifacts()
            .run_versions(run)
            .unwrap()
            .versions
            .is_empty()
    );
    resumed_after_the_interruption(&fixture.agent, &fixture.client, run);
    let resumed_session = active_text_session(&fixture, run);
    assert_ne!(session, resumed_session);
    let attempts = fixture.agent.runs_db().attempts(run).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1].generation, first.generation.next());
    assert_ne!(attempts[1].attempt, first.attempt);
    cancel_probe(&fixture, run);
    eprintln!(
        "live_recovery provider=codex version={} interrupted=true implicit_attempts=0 explicit_attempts=1 cleanup=confirmed native_resume=unsupported",
        fixture.version
    );
}

#[test]
fn live_scoped_endpoint_rejects_unknown_cross_run_and_revoked_tokens_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    // Starts an isolated Koru root only; no provider job is submitted.
    let fixture = live_fixture(&CODEX);
    let run = RunId::mint();
    let issued = fixture
        .client
        .artifacts()
        .issue_run_authority(&koru_artifact_wire::authority::IssueAuthorityRequest {
            run,
            workspace: fixture.request.workspace,
            generation: koru_delegate_core::AttemptGeneration::FIRST,
            grants: std::collections::BTreeSet::from([fixture.request.workspace]),
            run_workspace: koru_artifact_wire::authority::RunWorkspace::new(&fixture.directory)
                .unwrap(),
        })
        .expect("issue synthetic endpoint authority");
    let http = reqwest::blocking::Client::builder()
        .timeout(PROMPT)
        .no_proxy()
        .build()
        .unwrap();
    let send = |url: &str, token: &str| {
        http.post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/list", "params":{}
            }))
            .send()
            .unwrap_or_else(|_| panic!("isolated endpoint transport failed"))
    };
    let response = send(&issued.url, &issued.token);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response
        .json()
        .unwrap_or_else(|_| panic!("endpoint JSON invalid"));
    let offered: std::collections::BTreeSet<_> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        koru_artifact_wire::run_tools::TOOL_NAMES
            .iter()
            .copied()
            .collect()
    );
    let refused = |url: &str, token: &str, expected: &str| {
        let response = send(url, token);
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = response
            .json()
            .unwrap_or_else(|_| panic!("refusal JSON invalid"));
        assert!(
            body["code"].as_str() == Some(expected),
            "expected closed authority refusal code"
        );
    };
    refused(
        &issued.url,
        "koru_run_unknownfixturetoken",
        "authority_unknown",
    );
    let foreign = issued
        .url
        .replace(&run.to_storage(), &RunId::mint().to_storage());
    refused(&foreign, &issued.token, "authority_unknown");
    fixture
        .client
        .artifacts()
        .revoke_run_authority(run)
        .unwrap();
    refused(&issued.url, &issued.token, "authority_revoked");
    eprintln!(
        "live_endpoint scoped_tools=6 unknown=refused wrong_run=refused revoked=refused provider_calls=0"
    );
}
