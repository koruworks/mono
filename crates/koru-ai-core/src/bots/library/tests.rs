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
    assert!(validate_knowledge(&[page], home).is_ok());
}
