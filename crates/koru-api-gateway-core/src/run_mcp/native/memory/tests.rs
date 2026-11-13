// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;
use std::cell::RefCell;

#[test]
fn memory_tool_schema_rejects_extra_owner_and_invalid_cursor() {
    for args in [
        json!({"owner":"x"}),
        json!({"limit":0}),
        json!({"limit":21}),
        json!({"cursor":""}),
        json!({"cursor":"bad="}),
        json!({"cursor":"a".repeat(2049)}),
    ] {
        assert!(parse("list_memory", args).is_err());
    }
    let id = "00000000-0000-5000-8000-000000000001";
    assert!(parse("read_memory", json!({"fact_id":id})).is_ok());
    assert!(parse("read_memory", json!({"fact_id":id,"owner":"x"})).is_err());
    assert!(
        parse(
            "read_memory",
            json!({"fact_id":"AAAAAAAA-0000-5000-8000-000000000001"})
        )
        .is_err()
    );
    for tool in definitions() {
        assert!(tool.annotations.as_ref().unwrap().read_only_hint.unwrap());
    }
}
#[test]
fn memory_captured_default_and_current_are_forwarded_exactly() {
    assert_eq!(
        parse("list_memory", json!({})).unwrap(),
        MemoryReadOperation::List {
            view: MemoryView::Captured,
            cursor: None,
            limit: 20
        }
    );
    assert_eq!(
        parse(
            "list_memory",
            json!({"view":"current","limit":3,"cursor":"exact_cursor"})
        )
        .unwrap(),
        MemoryReadOperation::List {
            view: MemoryView::Current,
            cursor: Some("exact_cursor".into()),
            limit: 3
        }
    );
    assert_eq!(definitions().len(), 2);
    assert!(BOT_MEMORY_AVAILABLE);
    for name in ["list_memory", "read_memory"] {
        assert!(
            super::super::definitions()
                .iter()
                .any(|tool| tool.name == name)
        );
    }
}
#[test]
fn memory_complete_escaped_call_tool_result_fits_16k() {
    let oversized = failure("context_too_large");
    assert_eq!(oversized.code, "context_too_large");
    assert!(oversized.message.contains("read limit"));
    let page = BotMemoryListPage::Observed {
        page: BotPage {
            workspace: koru_store_core::ids::WorkspaceId::new(),
            document: koru_ai_core::DocumentId::new(),
        },
        version: koru_delegate_core::DocumentVersionId::new(),
        counts: BotMemoryCounts {
            valid: 1,
            omitted_from_prompt: 0,
            invalid: 0,
            not_indexed: 0,
        },
        items: vec![BotMemoryFactSummary {
            fact_id: BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4()),
            summary: "\"\\\n".repeat(14000),
        }],
        complete: true,
        continuation: None,
    };
    let bounded = result(Ok(RunMemoryReadResult::List(page)));
    assert_eq!(bounded.is_error, Some(true));
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= BOT_MEMORY_RESPONSE_BYTES);
    let absent = result(Ok(RunMemoryReadResult::List(BotMemoryListPage::Absent {})));
    assert_ne!(absent.is_error, Some(true));
}

struct Port {
    evidence: NativePolicyEvidence,
    seen: RefCell<Vec<RunMemoryReadRequest>>,
    fail: bool,
}
impl ReadPort for Port {
    fn policy(&self, _: NativePolicyRequest) -> Result<NativePolicyEvidence, String> {
        Ok(self.evidence.clone())
    }
    fn read(&self, request: RunMemoryReadRequest) -> Result<RunMemoryReadResult, String> {
        self.seen.borrow_mut().push(request);
        if self.fail {
            Err("ai_worker_timeout".into())
        } else {
            Ok(RunMemoryReadResult::List(BotMemoryListPage::Absent {}))
        }
    }
}
fn fixture() -> (Port, RunAuthorityRecord, BotProvenance, CapturedBotPolicy) {
    use koru_delegate_core::*;
    let workspace = WorkspaceId::new();
    let record = RunAuthorityRecord {
        run: RunId::mint(),
        workspace,
        generation: AttemptGeneration::FIRST,
        grants: [workspace].into(),
        run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
            if cfg!(windows) {
                "D:/isolated"
            } else {
                "/isolated"
            },
        ))
        .unwrap(),
        state: koru_artifact_core::AuthorityState::Active,
        created_at_ms: 0,
        revoked_at_ms: None,
    };
    let provenance = BotProvenance {
        routine: None,
        owner: BotOwner {
            user_scope: koru_store_core::UserScopeId::new(),
            device: DeviceId::new(),
        },
        bot: BotId::mint(),
        submission: BotRequestId::mint(),
        chat_id: uuid::Uuid::new_v4(),
        ai_attempt_id: None,
        assistant_message_id: None,
        config_revision: 1,
        policy_revision: 1,
        continued_from: None,
    };
    let captured = CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: BotAccess::Default,
        selected_pages: Default::default(),
    };
    let evidence = NativePolicyEvidence {
        profile: koru_ai_core::ProfileId::new(),
        barrier: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner: provenance.owner,
                bot: provenance.bot,
                request: uuid::Uuid::new_v4().try_into().unwrap(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: captured.clone(),
            effects_allowed: false,
        },
    };
    (
        Port {
            evidence,
            seen: RefCell::new(vec![]),
            fail: false,
        },
        record,
        provenance,
        captured,
    )
}
#[test]
fn memory_read_preserves_generation_and_needs_no_effects_on_fence() {
    let (port, record, provenance, captured) = fixture();
    let operation = parse("list_memory", json!({"view":"current"})).unwrap();
    assert!(
        invoke(
            &port,
            &record,
            provenance.clone(),
            captured.clone(),
            operation.clone(),
            Cancellation::default()
        )
        .is_ok()
    );
    let seen = port.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].generation, record.generation);
    assert_eq!(seen[0].run, record.run);
    assert_eq!(seen[0].provenance, provenance);
    assert_eq!(seen[0].captured, captured);
    assert_eq!(seen[0].operation, operation);
}
#[test]
fn memory_timeout_is_read_unavailable_and_cancels() {
    let (mut port, record, provenance, captured) = fixture();
    port.fail = true;
    let token = Cancellation::default();
    let error = invoke(
        &port,
        &record,
        provenance,
        captured,
        parse("list_memory", json!({})).unwrap(),
        token.clone(),
    )
    .unwrap_err();
    assert_eq!(error.code, "memory_read_unavailable");
    assert!(token.is_cancelled());
}

#[test]
fn memory_foreign_policy_refuses_before_worker_read() {
    let (mut port, record, provenance, captured) = fixture();
    port.evidence.barrier.key.bot = BotId::mint();
    let error = invoke(
        &port,
        &record,
        provenance,
        captured,
        parse("list_memory", json!({})).unwrap(),
        Cancellation::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "permission_denied");
    assert!(port.seen.borrow().is_empty());
}
