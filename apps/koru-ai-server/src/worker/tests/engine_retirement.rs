// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn ordinary_ai_has_no_bot_capabilities() {
    let mut config = WorkerConfig::new(
        "unused-memory-fixture".into(),
        "internal-debug",
        ProviderRegistry::default(),
    );
    config.test_repository = Some(koru_ai_sqlite::Repository::in_memory().unwrap());
    let worker = AiWorker::new(config).spawn().unwrap();
    worker.ping().unwrap();
    assert!(
        worker
            .info()
            .unwrap()
            .capabilities
            .iter()
            .all(|capability| !capability.starts_with("bots."))
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn retired_chats_are_hidden_and_rejected_even_with_manage_bots_scope() {
    let chat_id = ChatId::new();
    let mut repository = Repository::in_memory().unwrap();
    repository
        .create_chat(&Chat {
            id: chat_id,
            title: "Retained Bot".into(),
            private: false,
            current_model: model(),
            branched_from: None,
            agent_working_folder: None,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            bot: None,
            created_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(1),
        })
        .unwrap();
    repository.retire_chat_for_fixture(chat_id).unwrap();
    let (returned, database) = sync_channel(1);
    let mut config =
        WorkerConfig::deterministic_mock("unused-memory-fixture".into(), "internal-debug");
    config.test_repository = Some(repository);
    config.test_repository_return = Some(returned);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let mut owner = context();
    owner.principal.scopes.push(AiScope::ManageBots);

    assert_eq!(
        client.read_chat(owner.clone(), chat_id).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        client
            .rename_chat(
                owner.clone(),
                chat_id,
                RenameChatRequest {
                    title: "Changed".into()
                }
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        client.delete_chat(owner.clone(), chat_id).unwrap_err().code,
        "not_found"
    );
    let send = client.send_turn(
        owner.clone(),
        SendTurnRequest {
            destination_workspace: None,
            mini_app_host: None,
            mini_app_refinement: None,
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "This must never reach inference".into(),
            explicit_refs: vec![],
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: vec![],
            answers: None,
            skills: vec![],
        },
    );
    assert!(matches!(send, Err(error) if error.code == "not_found"));
    let listed = client.list_chats(owner.clone(), None).unwrap();
    assert!(listed.chats.is_empty());

    let ordinary = create_chat(&client);
    let stream = super::send(&client, ordinary, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    assert_eq!(client.clear_history(owner).unwrap().chats_deleted, 1);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = database.recv().unwrap();
    assert!(repository.messages(chat_id).unwrap().is_empty());
    assert!(repository.is_retired_bot_chat(chat_id).unwrap());
}
