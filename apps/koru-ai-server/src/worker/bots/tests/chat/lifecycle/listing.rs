// SPDX-License-Identifier: MPL-2.0
//! What a client that knows only the workspace can find after a restart:
//! the Bots in the Trash filter with the revision a Restore needs, and the
//! conversations in Trash with the operation that put each there.
use super::trash::{
    another_chat, no_owner_effects, restore_at, restore_chat_at, trash_at, trash_chat_at,
};
use super::*;

fn trashed_chats(client: &AiWorkerClient, workspace: WorkspaceId) -> Vec<BotTrashedChat> {
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let BotResponse::TrashedChats { page } = client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace,
                    cursor: cursor.clone(),
                    limit: 1,
                },
            )
            .unwrap()
        else {
            panic!("Trash page")
        };
        found.extend(page.chats);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return found,
        }
    }
}

fn trashed_bots(client: &AiWorkerClient, workspace: WorkspaceId) -> Vec<Bot> {
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace,
            lifecycle: BotLifecycle::Trashed,
        },
    ) else {
        panic!("Trash filter")
    };
    bots
}

#[test]
fn after_a_restart_the_workspace_alone_finds_and_restores_a_trashed_bot_and_its_conversations() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let alone = another_chat(&mut repository, &authority, &bot);
    let other = another_chat(&mut repository, &authority, &bot);
    let port = Arc::new(Archiving::default());
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    // A conversation trashed on its own, then the whole Bot.
    let chat_trash = BotRequestId::mint();
    trash_chat_at(&client, &bot, alone, chat_trash, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, chat_trash).stage,
        BotLifecycleStage::Completed
    );
    let bot_trash = BotRequestId::mint();
    trash_at(&client, &bot, bot_trash, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, bot_trash).stage,
        BotLifecycleStage::Completed
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    // A fresh worker over the same database; the client remembers nothing.
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let workspace = bot.home_workspace;
    let bots = trashed_bots(&client, workspace);
    assert_eq!(bots.iter().map(|b| b.id).collect::<Vec<_>>(), vec![bot.id]);
    // The revision a Restore needs, since the trashed Bot's read is
    // not_found.
    let revision = bots[0].revision;
    assert_eq!(
        client
            .bots(context(true), BotCommand::Read { bot_id: bot.id })
            .unwrap_err()
            .code,
        "not_found"
    );
    let listed = trashed_chats(&client, workspace);
    assert_eq!(listed.len(), 3, "{listed:?}");
    let entry = |chat: ChatId| {
        listed
            .iter()
            .find(|e| e.conversation.chat.id == chat)
            .unwrap()
            .clone()
    };
    // The conversations the Bot's Trash hid name it, and are restored with
    // the Bot; the one trashed before names its own operation.
    for chat in [bot.main_chat, other] {
        let found = entry(chat);
        assert_eq!(found.trashed_by.request_id, bot_trash);
        assert_eq!(found.trashed_by.kind, BotLifecycleKind::Trash);
        assert_eq!(found.restore, BotChatRestore::Bot);
        assert_eq!(found.conversation.label.bot_id, bot.id);
    }
    let own = entry(alone);
    assert_eq!(own.trashed_by.request_id, chat_trash);
    assert_eq!(own.trashed_by.kind, BotLifecycleKind::TrashChat);
    assert_eq!(own.restore, BotChatRestore::Bot, "its Bot is in Trash");
    // Restore, naming only what was read.
    let source = entry(bot.main_chat).trashed_by.request_id;
    restore_at(&client, &bot, source, revision).unwrap();
    assert!(trashed_bots(&client, workspace).is_empty());
    // The reversed operation is no longer named; the conversation trashed
    // on its own survives the Bot's Restore and is now restored on its own.
    let listed = trashed_chats(&client, workspace);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].conversation.chat.id, alone);
    assert_eq!(listed[0].restore, BotChatRestore::Conversation);
    let current = read_bot(&client, bot.id);
    restore_chat_at(
        &client,
        &bot,
        alone,
        listed[0].trashed_by.request_id,
        current.revision,
    )
    .unwrap();
    assert!(trashed_chats(&client, workspace).is_empty());
    // A workspace the caller cannot reach is refused, never listed.
    assert!(
        client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace: WorkspaceId::new(),
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(returned);
}

fn listed(client: &AiWorkerClient, workspace: WorkspaceId, lifecycle: BotLifecycle) -> Vec<Bot> {
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace,
            lifecycle,
        },
    ) else {
        panic!("lifecycle filter")
    };
    bots
}

/// A second Bot of the same owner and workspace.
fn second_bot(repository: &mut Repository, authority: &Arc<Authority>, create: &BotCommand) -> Bot {
    let BotCommand::Create {
        home_workspace,
        config,
        access,
        selected_pages,
        ..
    } = create.clone()
    else {
        panic!("create")
    };
    created(
        repository,
        authority.clone(),
        BotCommand::Create {
            request_id: BotRequestId::mint(),
            bot_id: BotId::mint(),
            home_workspace,
            config,
            access,
            selected_pages,
        },
    )
}

/// The archived and trashed Bots name the operation that holds each, with
/// the revision to send, so after a restart a client that knows only the
/// workspace unarchives and restores them with nothing it remembered.
#[test]
fn after_a_restart_the_workspace_alone_unarchives_and_restores_bots_by_their_holders() {
    let (mut repository, authority, create) = fixture();
    let archived = created(&mut repository, authority.clone(), create.clone());
    let trashed = second_bot(&mut repository, &authority, &create);
    let active = second_bot(&mut repository, &authority, &create);
    let port = Arc::new(Archiving::default());
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let workspace = archived.home_workspace;
    // One Bot archived; the other archived, then trashed.
    let first_archive = BotRequestId::mint();
    archive(&client, &archived, first_archive);
    assert_eq!(
        settled(&client, archived.id, first_archive).stage,
        BotLifecycleStage::Completed
    );
    let second_archive = BotRequestId::mint();
    archive(&client, &trashed, second_archive);
    assert_eq!(
        settled(&client, trashed.id, second_archive).stage,
        BotLifecycleStage::Completed
    );
    let in_archive = read_bot(&client, trashed.id);
    assert_eq!(
        in_archive.held_by.as_ref().map(|h| (h.request_id, h.kind)),
        Some((second_archive, BotLifecycleKind::Archive))
    );
    let trash = BotRequestId::mint();
    trash_at(&client, &trashed, trash, in_archive.revision).unwrap();
    assert_eq!(
        settled(&client, trashed.id, trash).stage,
        BotLifecycleStage::Completed
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    // A fresh worker over the same database; the client remembers nothing.
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    // An active Bot names no holder, and its bytes carry no field for one.
    let roster = match client
        .bots(context(true), BotCommand::List { workspace })
        .unwrap()
    {
        BotResponse::List { bots } => bots,
        _ => panic!("roster"),
    };
    assert_eq!(
        roster.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![active.id]
    );
    assert_eq!(roster[0].held_by, None);
    assert!(
        serde_json::to_value(&roster[0])
            .unwrap()
            .get("held_by")
            .is_none()
    );
    // The Archived filter: the Archive that holds it, and the revision.
    let in_archive = listed(&client, workspace, BotLifecycle::Archived);
    assert_eq!(in_archive.len(), 1, "{in_archive:?}");
    let holder = in_archive[0].held_by.clone().expect("held");
    assert_eq!(
        (in_archive[0].id, holder.request_id, holder.kind),
        (archived.id, first_archive, BotLifecycleKind::Archive)
    );
    assert!(holder.completed_at_ms > 0);
    // The archived Bot's own read says the same.
    assert_eq!(read_bot(&client, archived.id).held_by, Some(holder.clone()));
    // The Trash filter: the Trash holds it, not the Archive it was in.
    let in_trash = listed(&client, workspace, BotLifecycle::Trashed);
    assert_eq!(in_trash.len(), 1, "{in_trash:?}");
    let trash_holder = in_trash[0].held_by.clone().expect("held");
    assert_eq!(
        (in_trash[0].id, trash_holder.request_id, trash_holder.kind),
        (trashed.id, trash, BotLifecycleKind::Trash)
    );
    // Unarchive with only what was read.
    restore_at(
        &client,
        &archived,
        holder.request_id,
        in_archive[0].revision,
    )
    .unwrap();
    let back = read_bot(&client, archived.id);
    assert_eq!(back.lifecycle, BotLifecycle::Active);
    assert_eq!(back.held_by, None);
    // Restore from Trash with only what was read: back in the Archive, which
    // now holds it again; a reversed Trash is never named.
    restore_at(
        &client,
        &trashed,
        trash_holder.request_id,
        in_trash[0].revision,
    )
    .unwrap();
    assert!(listed(&client, workspace, BotLifecycle::Trashed).is_empty());
    let in_archive = listed(&client, workspace, BotLifecycle::Archived);
    assert_eq!(in_archive.len(), 1, "{in_archive:?}");
    assert_eq!(in_archive[0].id, trashed.id);
    let holder = in_archive[0].held_by.clone().expect("held");
    assert_eq!(
        (holder.request_id, holder.kind),
        (second_archive, BotLifecycleKind::Archive)
    );
    // And then unarchived, again from the read alone.
    restore_at(&client, &trashed, holder.request_id, in_archive[0].revision).unwrap();
    assert!(listed(&client, workspace, BotLifecycle::Archived).is_empty());
    assert_eq!(
        read_bot(&client, trashed.id).lifecycle,
        BotLifecycle::Active
    );
    // Another owner of the same workspace finds no holder: the filter is
    // refused once the caller cannot reach it, never answered with another's.
    authority.allowed.store(false, Ordering::SeqCst);
    assert!(
        client
            .bots(
                context(true),
                BotCommand::ListLifecycle {
                    workspace,
                    lifecycle: BotLifecycle::Archived,
                },
            )
            .is_err()
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(returned);
}

#[test]
fn another_owner_learns_nothing_of_a_workspaces_trash() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let alone = another_chat(&mut repository, &authority, &bot);
    let port = Arc::new(Archiving::default());
    let (worker, _returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, alone, request, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, request).stage,
        BotLifecycleStage::Completed
    );
    // Without Bot management the read is refused before anything is read.
    assert!(
        client
            .bots(
                context(false),
                BotCommand::ListTrashedBotChats {
                    workspace: bot.home_workspace,
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    // A workspace the owner cannot reach answers its refusal.
    authority.allowed.store(false, Ordering::SeqCst);
    assert!(
        client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace: bot.home_workspace,
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
