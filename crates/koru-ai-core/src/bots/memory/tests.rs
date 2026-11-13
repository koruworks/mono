// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::json;

fn fact() -> BotMemoryNodeId {
    "12345678-1234-5123-8123-123456789abc".parse().unwrap()
}
fn page() -> BotPage {
    BotPage {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
    }
}
fn commands() -> Vec<BotCommand> {
    let bot_id = BotId::mint();
    let creation_request = BotRequestId::mint();
    vec![
        BotCommand::ReadMemory { bot_id },
        BotCommand::EnsureMemory {
            request_id: creation_request,
            bot_id,
            expected_revision: 1,
            title: "Memory".into(),
        },
        BotCommand::RetryMemory {
            bot_id,
            creation_request,
            observed_state: BotMemoryState::Unconfirmed,
            observed_updated_at_ms: 4,
        },
        BotCommand::AbandonMemory {
            bot_id,
            creation_request,
            observed_state: BotMemoryState::Prepared,
            observed_updated_at_ms: 3,
        },
        BotCommand::ListMemory {
            bot_id,
            cursor: None,
            limit: 20,
        },
        BotCommand::ReadMemoryFact {
            bot_id,
            fact_id: fact(),
            cursor: None,
        },
    ]
}

#[test]
fn memory_commands_are_closed_and_preserve_observed_tokens_and_v5_facts() {
    for command in commands() {
        assert!(command.is_memory());
        let encoded = serde_json::to_value(&command).unwrap();
        assert_eq!(
            serde_json::from_value::<BotCommand>(encoded.clone()).unwrap(),
            command
        );
        let mut foreign = encoded;
        foreign["owner"] = json!("caller cannot choose ownership");
        assert!(serde_json::from_value::<BotCommand>(foreign).is_err());
    }
    let mut bad = serde_json::to_value(commands().pop().unwrap()).unwrap();
    bad["fact_id"] = json!("12345678-1234-5123-8123-123456789ABC");
    assert!(serde_json::from_value::<BotCommand>(bad).is_err());
    assert!(
        !BotCommand::ReadLibrary {
            bot_id: BotId::mint()
        }
        .is_memory()
    );
}

#[test]
fn memory_projection_empty_variants_reject_nested_unknown_fields() {
    assert!(
        serde_json::from_value::<BotMemoryListPage>(json!({"status":"absent","page":{}})).is_err()
    );
    assert!(
        serde_json::from_value::<BotMemoryDetailPage>(json!({"status":"absent","force":true}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<BotMemoryStatusState>(
            json!({"status":"not_started","can_create":true,"seed":"private"})
        )
        .is_err()
    );
    let candidate = BotMemoryStatusState::Candidate {
        creation_request: BotRequestId::mint(),
        candidate: page(),
        state: BotMemoryState::Conflict,
        observed_updated_at_ms: 7,
        refusal: Some(BotMemoryRefusal::DocumentChanged),
        can_retry: true,
        can_abandon: true,
    };
    let mut encoded = serde_json::to_value(candidate).unwrap();
    encoded["origin_edit_revision"] = json!([1]);
    assert!(serde_json::from_value::<BotMemoryStatusState>(encoded).is_err());
}

#[test]
fn memory_responses_preserve_typed_detail_and_snapshot_identity() {
    let page = page();
    let version = DocumentVersionId::new();
    let content = json!({"type":"text","text":"é","attributes":[{"role":"source","value":{"type":"url","value":"https://example.invalid/source"}}],"runs":[{"start_utf8":0,"end_utf8":2,"text":"é","bold":true}]});
    let responses = vec![
        BotResponse::MemoryStatus {
            status: BotMemoryStatus {
                bot_id: BotId::mint(),
                revision: 3,
                state: BotMemoryStatusState::NotStarted { can_create: false },
            },
        },
        BotResponse::MemoryList {
            page: BotMemoryListPage::Absent {},
        },
        BotResponse::MemoryList {
            page: BotMemoryListPage::Observed {
                page: page.clone(),
                version,
                counts: BotMemoryCounts {
                    valid: 1,
                    omitted_from_prompt: 0,
                    invalid: 2,
                    not_indexed: 3,
                },
                items: vec![BotMemoryFactSummary {
                    fact_id: fact(),
                    summary: "Fact".into(),
                }],
                complete: true,
                continuation: None,
            },
        },
        BotResponse::MemoryDetail {
            page: BotMemoryDetailPage::Observed {
                page,
                version,
                fact_id: fact(),
                chunks: vec![BotMemoryDetailChunk {
                    node_id: fact(),
                    parent_id: None,
                    kind: "paragraph".into(),
                    range: Some(BotMemoryTextRange {
                        start_utf8: 0,
                        end_utf8: 2,
                    }),
                    content,
                }],
                complete: false,
                continuation: Some("opaque_token".into()),
            },
        },
    ];
    for response in responses {
        assert_eq!(
            serde_json::from_slice::<BotResponse>(&serde_json::to_vec(&response).unwrap()).unwrap(),
            response
        );
    }
}

#[test]
fn memory_read_bounds_do_not_authenticate_a_cursor() {
    let operation = |limit, cursor| MemoryReadOperation::List {
        view: MemoryView::Current,
        limit,
        cursor,
    };
    assert!(operation(1, None).validate_bounds().is_ok());
    assert!(
        operation(20, Some("a".repeat(2048)))
            .validate_bounds()
            .is_ok()
    );
    for invalid in [
        Some(String::new()),
        Some("a".repeat(2049)),
        Some("a=".into()),
        Some("a\\b".into()),
        Some("é".into()),
    ] {
        assert!(operation(20, invalid).validate_bounds().is_err());
    }
    assert!(operation(0, None).validate_bounds().is_err());
    assert!(operation(21, None).validate_bounds().is_err());
    // This is only an alphabet/size check; a real read still verifies its MAC.
    assert!(
        operation(20, Some("unsigned".into()))
            .validate_bounds()
            .is_ok()
    );
}

#[test]
fn memory_capture_shape_requires_exact_binding_and_bounded_unique_nodes() {
    let binding = BotMemoryBinding {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::new(),
        },
        bot: BotId::mint(),
        page: page(),
        creation_request: BotRequestId::mint(),
        bound_at_ms: 1,
    };
    let mut capture = PreparedMemoryCapture {
        owner: binding.owner,
        bot: binding.bot,
        creation_request: binding.creation_request,
        page: binding.page.clone(),
        version: DocumentVersionId::new(),
        nodes: vec![fact()],
        omitted_count: 0,
        invalid_count: 0,
        index_digest: [0; 32],
    };
    assert!(capture.validate_for_binding(&binding).is_ok());
    capture.nodes.push(fact());
    assert!(capture.validate_for_binding(&binding).is_err());
    capture.nodes.truncate(1);
    capture.invalid_count = u64::MAX;
    assert!(capture.validate_for_binding(&binding).is_err());
    capture.invalid_count = 0;
    capture.creation_request = BotRequestId::mint();
    assert!(capture.validate_for_binding(&binding).is_err());
}

#[test]
fn memory_index_shape_never_accepts_a_body_for_absent() {
    let home = WorkspaceId::new();
    let mut index = VerifiedMemoryIndex {
        capture: BotLibraryCapture {
            version: 1,
            owner: BotOwner {
                user_scope: UserScopeId::new(),
                device: DeviceId::new(),
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            config_revision: 1,
            knowledge: vec![],
            memory: BotCapturedMemory::Absent {},
            index_digest: [0; 32],
        },
        bytes: Vec::new(),
    };
    // Shape checks do not claim that this deliberately wrong digest is verified.
    assert!(index.validate_shape(home).is_ok());
    index.bytes.push(b'x');
    assert!(index.validate_shape(home).is_err());
    index.bytes = vec![b'x'; BOT_MEMORY_INDEX_BYTES + 1];
    assert!(index.validate_shape(home).is_err());
}

#[cfg(feature = "schema")]
#[test]
fn memory_schema_keeps_canonical_uuid_domain_and_state_vocabulary() {
    let node = serde_json::to_value(schemars::schema_for!(BotMemoryNodeId)).unwrap();
    assert_eq!(
        node["pattern"],
        "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    );
    let state = serde_json::to_value(schemars::schema_for!(BotMemoryState)).unwrap();
    assert!(
        state["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("unconfirmed"))
    );
    let refusal = serde_json::to_value(schemars::schema_for!(BotMemoryRefusal)).unwrap();
    assert!(
        refusal["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("document_changed"))
    );
    let _ = schemars::schema_for!(BotResponse);
}
