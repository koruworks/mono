// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::library::BotKnowledgeReceipt;

fn fixture() -> (Library, Bot) {
    let bot = crate::bots::tests::bot(
        BotId::mint(),
        koru_store_core::ids::WorkspaceId::new(),
        "Scout",
        false,
    );
    let mut library = Library::default();
    library.know_bot(&bot);
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    assert!(library.answer_read(seq, &bot, Ok(view(&bot, vec![]))));
    (library, bot)
}
fn page(bot: &Bot) -> BotPage {
    BotPage {
        workspace: bot.home_workspace,
        document: koru_ai_core::DocumentId::new(),
    }
}
fn view(bot: &Bot, knowledge: Vec<BotPage>) -> BotLibraryView {
    BotLibraryView {
        bot_id: bot.id,
        revision: bot.revision,
        role: bot.config.role.clone(),
        knowledge,
        memory: None,
    }
}
fn sent(library: &mut Library, bot: &Bot) -> (BotRequestId, BotCommand) {
    let Command::Write {
        request, command, ..
    } = library.save(bot).unwrap()
    else {
        panic!()
    };
    (request, *command)
}
fn saved(bot: &Bot, request_id: BotRequestId, pages: Vec<BotPage>) -> BotResponse {
    BotResponse::KnowledgeSaved {
        receipt: BotKnowledgeReceipt {
            bot_id: bot.id,
            request_id,
            revision: bot.revision + 1,
            pages,
        },
    }
}

#[test]
fn library_unknown_reply_preserves_exact_body_and_separate_draft() {
    let (mut library, bot) = fixture();
    let a = page(&bot);
    assert!(library.add(&bot, a.clone()));
    let (request, command) = sent(&mut library, &bot);
    library.answer_save(request, Err(Failure::Unanswered));
    assert!(
        !library.add(&bot, page(&bot)),
        "uncertain command remains immutable"
    );
    assert!(!library.discard());
    assert_eq!(sent(&mut library, &bot), (request, command));
    library.answer_save(request, Ok(saved(&bot, request, vec![a.clone()])));
    assert_eq!(library.pages(), &[a]);
    assert!(library.saved());
}

#[test]
fn library_unsent_read_is_unavailable_until_explicit_reload() {
    let (mut library, bot) = fixture();
    library.owe_read();
    assert!(library.read(bot.id).is_some());
    library.read_unsent();
    assert!(!library.loading());
    assert!(!library.awaiting());
    assert_eq!(library.notice(), Some(Words::BotsErrUnreachable));
    assert!(library.read(bot.id).is_none());
    library.owe_read();
    assert!(library.read(bot.id).is_some());
}

#[test]
fn library_ack_fences_stale_reads_and_old_retry_receipt_keeps_current_state() {
    let (mut library, mut bot) = fixture();
    let a = page(&bot);
    assert!(library.add(&bot, a.clone()));
    let (request, _) = sent(&mut library, &bot);
    library.owe_read();
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    library.answer_save(request, Ok(saved(&bot, request, vec![a.clone()])));
    assert!(!library.answer_read(seq, &bot, Ok(view(&bot, vec![]))));
    assert_eq!(library.pages(), &[a.clone()]);
    assert_eq!(library.known_revision(), bot.revision + 1);

    bot.revision += 1;
    library.know_bot(&bot);
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    library.answer_read(seq, &bot, Ok(view(&bot, vec![a.clone()])));
    let b = page(&bot);
    library.add(&bot, b.clone());
    let (request, command) = sent(&mut library, &bot);
    library.answer_save(request, Err(Failure::Unanswered));
    let old_bot = bot.clone();
    bot.revision += 2;
    library.know_bot(&bot);
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    library.answer_read(seq, &bot, Ok(view(&bot, vec![b.clone()])));
    assert_eq!(sent(&mut library, &bot), (request, command));
    library.answer_save(request, Ok(saved(&old_bot, request, vec![a, b.clone()])));
    assert_eq!(library.held().unwrap().revision, bot.revision);
    assert_eq!(
        library.pages(),
        &[b],
        "old receipt does not replace newer canonical pages"
    );
}

#[test]
fn library_conflict_preserves_draft_and_order_while_archived_is_read_only() {
    let (mut library, mut bot) = fixture();
    let a = page(&bot);
    let b = page(&bot);
    library.add(&bot, a.clone());
    library.add(&bot, b.clone());
    assert!(library.move_page(&bot, &b, true));
    assert_eq!(library.pages(), &[b.clone(), a.clone()]);
    let (request, _) = sent(&mut library, &bot);
    library.answer_save(request, Err(Failure::Refused("revision_conflict".into())));
    let draft = library.pages().to_vec();
    bot.revision += 1;
    library.know_bot(&bot);
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    library.answer_read(seq, &bot, Ok(view(&bot, vec![a])));
    assert_eq!(library.pages(), draft);
    assert_eq!(library.notice(), Some(Words::BotsChangedElsewhere));
    bot.lifecycle = BotLifecycle::Archived;
    assert!(!library.can_save(&bot));
    assert!(!library.remove(&bot, &b));
    assert!(!library.move_page(&bot, &b, false));
}

#[test]
fn library_membership_is_home_only_bounded_and_excludes_bound_memory() {
    let (mut library, bot) = fixture();
    let memory = page(&bot);
    library.held.as_mut().unwrap().memory = Some(memory.clone());
    assert!(!library.add(&bot, memory));
    let mut foreign = page(&bot);
    foreign.workspace = koru_store_core::ids::WorkspaceId::new();
    assert!(!library.add(&bot, foreign));
    for _ in 0..16 {
        assert!(library.add(&bot, page(&bot)));
    }
    assert!(!library.add(&bot, page(&bot)));
    assert!(!library.add(&bot, library.pages()[0].clone()));
}

#[test]
fn memory_binding_retries_exactly_and_preserves_role_and_knowledge_draft() {
    let (mut library, mut bot) = fixture();
    let knowledge = page(&bot);
    assert!(library.add(&bot, knowledge.clone()));
    let memory = page(&bot);
    let Command::Write {
        request, command, ..
    } = library.set_memory(&bot, Some(memory.clone())).unwrap()
    else {
        panic!()
    };
    assert!(
        matches!(&*command, BotCommand::SetMemory { expected_revision, memory: Some(sent), .. }
        if *expected_revision == bot.revision && sent == &memory)
    );
    library.answer_save(request, Err(Failure::Unanswered));
    assert!(library.set_memory(&bot, None).is_none());
    assert_eq!(sent(&mut library, &bot), (request, *command));
    bot.revision += 1;
    library.answer_save(request, Ok(BotResponse::Bot { bot: bot.clone() }));
    let held = library.held().unwrap();
    assert_eq!(held.memory, Some(memory));
    assert_eq!(held.role, bot.config.role);
    assert!(
        held.knowledge.is_empty(),
        "an unsaved Knowledge draft is not sent with Memory"
    );
    assert_eq!(library.pages(), &[knowledge]);
    assert!(library.dirty());
}

#[test]
fn memory_binding_fences_stale_reads_and_does_not_regress_a_newer_binding() {
    let (mut library, mut bot) = fixture();
    let older = page(&bot);
    let newer = page(&bot);
    let Command::Write { request, .. } = library.set_memory(&bot, Some(older)).unwrap() else {
        panic!()
    };
    library.answer_save(request, Err(Failure::Unanswered));
    let mut receipt = bot.clone();
    receipt.revision += 1;
    bot.revision += 2;
    library.know_bot(&bot);
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    let mut current = view(&bot, vec![]);
    current.memory = Some(newer.clone());
    assert!(library.answer_read(seq, &bot, Ok(current)));
    library.owe_read();
    let Command::ReadLibrary { seq, .. } = library.read(bot.id).unwrap() else {
        panic!()
    };
    library.answer_save(request, Ok(BotResponse::Bot { bot: receipt }));
    assert!(!library.answer_read(seq, &bot, Ok(view(&bot, vec![]))));
    assert_eq!(library.held().unwrap().memory, Some(newer));
    assert_eq!(library.held().unwrap().revision, bot.revision);
}

#[test]
fn memory_binding_rejects_role_knowledge_other_workspace_and_archived_changes() {
    let (mut library, mut bot) = fixture();
    assert!(
        library
            .set_memory(&bot, Some(bot.config.role.clone()))
            .is_none()
    );
    let knowledge = page(&bot);
    library.add(&bot, knowledge.clone());
    assert!(library.set_memory(&bot, Some(knowledge)).is_none());
    let mut foreign = page(&bot);
    foreign.workspace = koru_store_core::ids::WorkspaceId::new();
    assert!(library.set_memory(&bot, Some(foreign)).is_none());
    bot.lifecycle = BotLifecycle::Archived;
    assert!(library.set_memory(&bot, Some(page(&bot))).is_none());
}
