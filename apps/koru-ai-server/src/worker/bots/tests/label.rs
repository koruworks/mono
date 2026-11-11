// SPDX-License-Identifier: MPL-2.0
//! `Chat.bot` through the real worker: the owner's listing and transcript
//! carry the label; an ordinary caller sees neither the label nor the chat.
use super::*;

fn worker(repo: Repository, authority: Arc<Authority>) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-label"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

#[test]
fn the_owner_reads_the_label_and_an_ordinary_caller_never_sees_the_chat() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (worker, client) = worker(repo, authority);
    let expected = Some(BotChatLabel {
        bot_id: bot.id,
        bot_name: bot.config.name.clone(),
        purpose: BotChatPurpose::Main,
    });

    let owner = client.list_chats(context(true), None).unwrap().chats;
    let main = owner
        .iter()
        .find(|chat| chat.id == bot.main_chat)
        .expect("the owner lists the Bot conversation");
    assert_eq!(main.bot, expected);
    assert_eq!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .chat
            .bot,
        expected
    );

    let ordinary = client.list_chats(context(false), None).unwrap().chats;
    assert!(ordinary.iter().all(|chat| chat.id != bot.main_chat));
    assert!(ordinary.iter().all(|chat| chat.bot.is_none()));
    assert_eq!(
        client
            .read_chat(context(false), bot.main_chat)
            .unwrap_err()
            .code,
        "not_found"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
