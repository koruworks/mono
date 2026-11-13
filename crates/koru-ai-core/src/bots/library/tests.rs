// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn library_api_contract_keeps_references_and_receipts_narrow() {
    let page = BotPage {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
    };
    let bot_id = BotId::mint();
    let request_id = BotRequestId::mint();
    let command = BotCommand::SetKnowledge {
        request_id,
        bot_id,
        expected_revision: 4,
        pages: vec![page.clone()],
    };
    let encoded = serde_json::to_value(&command).unwrap();
    assert_eq!(encoded["command"], "set_knowledge");
    assert_eq!(
        serde_json::from_value::<BotCommand>(encoded.clone()).unwrap(),
        command
    );
    let mut invalid = encoded;
    invalid["selected_pages"] = serde_json::json!([]);
    assert!(serde_json::from_value::<BotCommand>(invalid).is_err());
    let view = BotLibraryView {
        bot_id,
        revision: 4,
        role: page.clone(),
        knowledge: vec![page.clone()],
        memory: None,
    };
    let mut encoded = serde_json::to_value(&view).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 5);
    encoded["creation_request"] = serde_json::json!(request_id);
    assert!(serde_json::from_value::<BotLibraryView>(encoded).is_err());
    let receipt = BotKnowledgeReceipt {
        bot_id,
        request_id,
        revision: 5,
        pages: vec![page],
    };
    let response = BotResponse::KnowledgeSaved { receipt };
    assert_eq!(
        serde_json::from_slice::<BotResponse>(&serde_json::to_vec(&response).unwrap()).unwrap(),
        response
    );
}

#[test]
fn library_domain_rejects_unknown_and_unbounded() {
    let home = WorkspaceId::new();
    let page = BotPage {
        workspace: home,
        document: DocumentId::new(),
    };
    assert!(validate_knowledge(&vec![page.clone(); 17], home).is_err());
    assert!(validate_knowledge(&[page.clone(), page.clone()], home).is_err());
    assert!(validate_knowledge(&[page.clone()], WorkspaceId::new()).is_err());
    let receipt = BotLibraryReceiptV1::SetKnowledge {
        version: 1,
        revision: 2,
        pages: vec![page.clone()],
    };
    receipt.validate().unwrap();
    let mut json = serde_json::to_value(&receipt).unwrap();
    json["private"] = true.into();
    assert!(serde_json::from_value::<BotLibraryReceiptV1>(json).is_err());
    assert!(
        BotLibraryReceiptV1::SetKnowledge {
            version: 2,
            revision: 2,
            pages: vec![]
        }
        .validate()
        .is_err()
    );
    assert!(
        serde_json::from_str::<BotCapturedMemory>(r#"{"kind":"absent","extra":true}"#).is_err()
    );
    let mut pin = serde_json::to_value(BotKnowledgeCapture {
        page: page.clone(),
        version: DocumentVersionId::new(),
    })
    .unwrap();
    pin["version"] = serde_json::json!([1, 2, 3]);
    assert!(serde_json::from_value::<BotKnowledgeCapture>(pin).is_err());
    let owner = BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::new(),
    };
    let mut identity = BotMemoryIdentity {
        owner,
        bot: BotId::mint(),
        workspace: home,
        creation_request: BotRequestId::mint(),
        candidate: page.document,
        store_request: BotRequestId::mint(),
        seed_format: 1,
        seed_title: "é".repeat(128),
        seed_hash: [0; 32],
    };
    identity.validate().unwrap();
    identity.seed_title.push('a');
    assert!(identity.validate().is_err());
    identity.seed_title = "Memory".into();
    let mut creation = BotMemoryCreation {
        identity: identity.clone(),
        state: BotMemoryState::Prepared,
        origin_edit_revision: Some(vec![1]),
        refusal: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    assert!(creation.validate().is_err());
    creation.state = BotMemoryState::Bound;
    creation.validate().unwrap();
    creation.origin_edit_revision = Some(vec![0; 65537]);
    assert!(creation.validate().is_err());
    creation.origin_edit_revision = None;
    creation.state = BotMemoryState::Conflict;
    assert!(creation.validate().is_err());
    creation.refusal = Some(BotMemoryRefusal::DocumentChanged);
    creation.validate().unwrap();
    let mut capture = BotLibraryCapture {
        version: 1,
        owner,
        bot: identity.bot,
        submission: BotRequestId::mint(),
        config_revision: 1,
        knowledge: vec![],
        memory: BotCapturedMemory::Absent {},
        index_digest: [0; 32],
    };
    capture.validate(home).unwrap();
    capture.memory = BotCapturedMemory::Captured {
        page,
        version: DocumentVersionId::new(),
        creation_request: identity.creation_request,
        nodes: vec![BlockNodeId::mint(); 257],
        omitted_count: 0,
        invalid_count: 0,
    };
    assert!(capture.validate(home).is_err());
    let mut encoded = serde_json::to_value(&identity).unwrap();
    encoded["creation_request"] = "not-a-uuid".into();
    assert!(serde_json::from_value::<BotMemoryIdentity>(encoded).is_err());
    let evidence = BotMemoryConfirmation {
        identity,
        original_edit_revision: vec![1],
        live_edit_revision: vec![2],
    };
    assert!(evidence.validate().is_err());
}
