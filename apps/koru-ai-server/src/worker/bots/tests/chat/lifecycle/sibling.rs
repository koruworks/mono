// SPDX-License-Identifier: MPL-2.0
//! A message sent in one conversation of a Bot while another conversation
//! of it is trashed (the third lifecycle review's F4). Trashing a
//! conversation with work its owner had not admitted refuses that work
//! through a fence at the Bot's next revision; a sibling's Send prepared at
//! the old revision and submitted after the fence is refused `bot_stale`.
//! Nothing of it ran and nothing about it is wrong, so it is sent again by
//! itself, once, at the current revision, through the person's own Retry
//! path (`chat/resend.rs`): "its siblings keep running".
//!
//! The Send is held between its preparation and its Submit in its context
//! window (the one context thread held by an ordinary chat's document read,
//! `unadmitted::HeldContext`), which is where the race happens.
use super::trash::{another_chat, request_for, trash_chat_at};
use super::unadmitted::{HeldContext, hold_context_thread, wait_context_window};
use super::*;

struct Sibling {
    worker: WorkerHandle,
    client: AiWorkerClient,
    returned: Receiver<Repository>,
    port: Arc<Archiving>,
    reader: Arc<HeldContext>,
    bot: Bot,
    /// The conversation the person types in.
    chat: ChatId,
    /// Other conversations of the Bot, to send in and trash.
    others: Vec<ChatId>,
    authority: Arc<Authority>,
}

fn sibling(others: usize) -> Sibling {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = another_chat(&mut repository, &authority, &bot);
    // A first task already ran, so the working folder is bound and tasks of
    // several conversations may wait at once (the first-task folder lock
    // admits one).
    let ran = super::unadmitted::admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    let mut chats = vec![bot.main_chat];
    chats.extend((1..others).map(|_| another_chat(&mut repository, &authority, &bot)));
    let current = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let port = Arc::new(Archiving::default());
    port.capture_history.store(true, Ordering::SeqCst);
    port.api.lock().unwrap().push(ran);
    let reader = Arc::new(HeldContext::default());
    let (worker, returned) = spawn_with(
        repository,
        authority.clone(),
        port.clone(),
        Some(reader.clone() as Arc<dyn StoreContextPort>),
    );
    let client = worker.client();
    Sibling {
        worker,
        client,
        returned,
        port,
        reader,
        bot: current,
        chat,
        others: chats,
        authority,
    }
}

impl Sibling {
    /// A Send in `chat`, prepared now (at the Bot's current revision); it
    /// waits in its context window while the context thread is held.
    fn send_in(&self, chat: ChatId) -> InferenceAttemptId {
        let attempt = self
            .client
            .send_turn(context(true), request_for(&self.bot, chat))
            .unwrap()
            .accepted
            .attempt_id;
        wait_context_window(&self.client, attempt);
        attempt
    }

    /// The person sends in the sibling conversation, and someone sends in
    /// another conversation `other` too (the work its Trash will refuse);
    /// both wait in their context windows until the returned sender
    /// releases the context thread.
    fn send_held(&self, other: Option<usize>) -> SyncSender<()> {
        self.send_held_attempt(other).0
    }

    /// [`Self::send_held`], with the person's own Send's attempt.
    fn send_held_attempt(&self, other: Option<usize>) -> (SyncSender<()>, InferenceAttemptId) {
        let release = hold_context_thread(&self.client, &self.reader);
        let attempt = self.send_in(self.chat);
        if let Some(n) = other {
            self.send_in(self.others[n]);
        }
        (release, attempt)
    }

    /// Trash conversation `n` of `others` (its Send waiting in its context
    /// window is refused through a fence at the next revision), wait until
    /// that fence holds the owner at a newer epoch than the revision before
    /// it, then until the Trash completes.
    fn trash_other(&self, n: usize) {
        let revision = read_bot(&self.client, self.bot.id).revision;
        let request = BotRequestId::mint();
        trash_chat_at(&self.client, &self.bot, self.others[n], request, revision).unwrap();
        until("the Trash's fence reached the owner", || {
            self.port
                .barrier
                .lock()
                .unwrap()
                .get(&self.bot.id)
                .is_some_and(|b| b.epoch > revision)
        });
        let done = settled(&self.client, self.bot.id, request);
        assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    }

    fn submits(&self) -> usize {
        self.port.submits.lock().unwrap().len()
    }

    fn stop(
        self,
    ) -> (
        Repository,
        Arc<Archiving>,
        Arc<HeldContext>,
        Arc<Authority>,
        ChatId,
    ) {
        self.worker.shutdown();
        assert_eq!(self.worker.join(), WorkerExit::Clean);
        (
            self.returned.recv_timeout(Duration::from_secs(5)).unwrap(),
            self.port,
            self.reader,
            self.authority,
            self.chat,
        )
    }
}

/// The conversation's replies, oldest first. A chat read is proved against
/// the Bot's revision when it is prepared, so one prepared just before the
/// revision moves (a pause, a refusal fence) is asked again.
fn replies(client: &AiWorkerClient, chat: ChatId) -> Vec<MessageState> {
    let transcript = loop {
        match client.read_chat(context(true), chat) {
            Err(error) if error.code == BotError::RevisionConflict.code() => continue,
            read => break read.unwrap(),
        }
    };
    transcript
        .messages
        .iter()
        .filter(|m| m.role == ChatRole::Assistant)
        .map(|m| m.state)
        .collect()
}

/// Long enough for a re-send that should not happen to have begun.
fn quiet() {
    std::thread::sleep(Duration::from_millis(2500));
}

#[test]
fn a_sibling_message_refused_by_another_conversations_trash_is_sent_again_once() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    // Another conversation is trashed while the Send is on its way.
    run.trash_other(0);
    release.send(()).unwrap();
    until("the sibling's reply completed", || {
        replies(&run.client, run.chat).last() == Some(&MessageState::Complete)
    });
    assert_eq!(
        replies(&run.client, run.chat),
        vec![MessageState::Interrupted, MessageState::Complete]
    );
    assert_eq!(run.submits(), 2, "sent again once");
    quiet();
    assert_eq!(run.submits(), 2);
    let _ = run.stop();
}

/// The bound: a message sent again that is refused as stale once more (a
/// second conversation trashed at that very moment) is not sent a third
/// time. One automatic re-send per message, ever.
#[test]
fn a_message_sent_again_is_never_sent_a_third_time() {
    let run = sibling(2);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    // The re-send will wait in its own context window: the context thread
    // is held again as soon as the first Send has passed it.
    until("the first Send reached the owner", || run.submits() == 1);
    let release_again = hold_context_thread(&run.client, &run.reader);
    until("the re-send waits in its context window", || {
        replies(&run.client, run.chat) == vec![MessageState::Interrupted, MessageState::Pending]
    });
    run.send_in(run.others[1]);
    run.trash_other(1);
    release_again.send(()).unwrap();
    until("the re-sent reply ended", || {
        replies(&run.client, run.chat) == vec![MessageState::Interrupted, MessageState::Interrupted]
    });
    quiet();
    assert_eq!(run.submits(), 2);
    assert_eq!(replies(&run.client, run.chat).len(), 2);
    let _ = run.stop();
}

/// A message of the conversation being trashed is settled by that Trash
/// (its receipt carries the Trash's refusal from the capture on, its
/// attempt is stopped as the person's Stop): it is never sent again into a
/// conversation in Trash.
#[test]
fn a_message_of_the_conversation_being_trashed_is_not_sent_again() {
    let run = sibling(1);
    let release = run.send_held(None);
    let revision = read_bot(&run.client, run.bot.id).revision;
    let request = BotRequestId::mint();
    trash_chat_at(&run.client, &run.bot, run.chat, request, revision).unwrap();
    release.send(()).unwrap();
    let done = settled(&run.client, run.bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    quiet();
    assert_eq!(run.submits(), 0);
    let _ = run.stop();
}

/// A Bot the person paused while the message was on its way: nothing of it
/// is sent again into the paused Bot.
#[test]
fn a_message_on_its_way_while_its_bot_is_paused_is_not_sent_again() {
    let run = sibling(1);
    let release = run.send_held(None);
    run.client
        .bots(
            context(true),
            BotCommand::Pause {
                request_id: BotRequestId::mint(),
                bot_id: run.bot.id,
                expected_revision: read_bot(&run.client, run.bot.id).revision,
            },
        )
        .unwrap();
    until("the owner holds the Bot paused", || {
        run.port
            .barrier
            .lock()
            .unwrap()
            .get(&run.bot.id)
            .is_some_and(|b| b.paused)
    });
    release.send(()).unwrap();
    until("the reply ended", || {
        replies(&run.client, run.chat) == vec![MessageState::Interrupted]
    });
    quiet();
    assert!(run.submits() <= 1);
    assert_eq!(
        replies(&run.client, run.chat),
        vec![MessageState::Interrupted]
    );
    let _ = run.stop();
}

/// The person's own Retry of the conversation's last reply.
fn persons_retry(client: &AiWorkerClient, bot: &Bot, chat: ChatId) {
    let transcript = loop {
        match client.read_chat(context(true), chat) {
            Err(error) if error.code == BotError::RevisionConflict.code() => continue,
            read => break read.unwrap(),
        }
    };
    let last = transcript
        .messages
        .iter()
        .rev()
        .find(|m| m.role == ChatRole::Assistant)
        .unwrap()
        .id;
    client
        .retry_turn(
            context(true),
            chat,
            RetryTurnRequest {
                destination_workspace: Some(bot.home_workspace),
                operation_id: Uuid::new_v4(),
                message_id: last,
                surface: AiSurface::Standalone,
                explicit_refs: vec![],
                reasoning_override: None,
                web_search: false,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
}

/// The first Submit's request, once the refusal of it is final.
fn refused_for_good(run: &Sibling) -> BotRequestId {
    until("the Send reached the owner", || run.submits() == 1);
    let request = run
        .port
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|(_, what, _)| *what == "submit")
        .and_then(|(_, _, request)| *request)
        .unwrap();
    until("the refusal is final", || {
        super::unadmitted::submission_status(&run.client, run.bot.id, request)
            == BotOperationStatus::Rejected
    });
    request
}

/// The AI worker, restarted on the same database, owner and context reader:
/// it sends nothing by itself, the refused reply stays as it ended, and the
/// person's Retry of it is sent and completes.
fn after_a_restart_only_the_persons_retry_sends(run: Sibling) {
    let bot = run.bot.clone();
    let (repository, port, reader, authority, chat) = run.stop();
    port.unavailable.store(false, Ordering::SeqCst);
    let (worker, _returned) = spawn_with(
        repository,
        authority,
        port.clone(),
        Some(reader as Arc<dyn StoreContextPort>),
    );
    let client = worker.client();
    quiet();
    assert_eq!(
        port.submits.lock().unwrap().len(),
        1,
        "a restart sent nothing by itself"
    );
    assert_eq!(replies(&client, chat), vec![MessageState::Interrupted]);
    persons_retry(&client, &bot, chat);
    until("the person's Retry completed", || {
        replies(&client, chat).last() == Some(&MessageState::Complete)
    });
    assert_eq!(port.submits.lock().unwrap().len(), 2);
    assert_eq!(
        replies(&client, chat),
        vec![MessageState::Interrupted, MessageState::Complete]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// The AI worker restarts after the refusal and before the re-send: a
/// re-send is owed only in the run that saw the refusal, so the new worker
/// sends nothing by itself (the accepted cost of the fourth lifecycle
/// review's F1), and the person's Retry sends it.
#[test]
fn a_restart_before_the_re_send_sends_nothing_and_the_persons_retry_does() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    // The owner goes silent the moment the refusal is final, so the re-send
    // cannot be asked for before the restart.
    refused_for_good(&run);
    run.port.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(run.submits(), 1, "nothing was sent again yet");
    after_a_restart_only_the_persons_retry_sends(run);
}

/// The fourth lifecycle review's proof, inverted. A message refused as stale
/// whose automatic re-send its Bot refused (the person paused the Bot) stays
/// dropped: the person's Resume sends nothing in that run, and a restart of
/// the AI worker after it sends nothing either (it once sent the old
/// message by itself). The person's Retry still sends it.
#[test]
fn a_stale_re_send_a_pause_refused_stays_dropped_through_resume_and_a_restart() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    refused_for_good(&run);
    // The owner goes silent so the re-send cannot be asked for yet, and the
    // person pauses the Bot meanwhile.
    run.port.unavailable.store(true, Ordering::SeqCst);
    run.client
        .bots(
            context(true),
            BotCommand::Pause {
                request_id: BotRequestId::mint(),
                bot_id: run.bot.id,
                expected_revision: read_bot(&run.client, run.bot.id).revision,
            },
        )
        .unwrap();
    run.port.unavailable.store(false, Ordering::SeqCst);
    until("the owner holds the Bot paused", || {
        run.port
            .barrier
            .lock()
            .unwrap()
            .get(&run.bot.id)
            .is_some_and(|b| b.paused)
    });
    until("the pause is delivered", || {
        read_bot(&run.client, run.bot.id).pending_change.is_none()
    });
    quiet();
    quiet();
    assert_eq!(run.submits(), 1, "the paused Bot refused the re-send");
    assert_eq!(
        replies(&run.client, run.chat),
        vec![MessageState::Interrupted]
    );
    // The person resumes the Bot: nothing is sent by itself.
    run.client
        .bots(
            context(true),
            BotCommand::Resume {
                request_id: BotRequestId::mint(),
                bot_id: run.bot.id,
                expected_revision: read_bot(&run.client, run.bot.id).revision,
            },
        )
        .unwrap();
    until("the owner holds the Bot resumed", || {
        run.port
            .barrier
            .lock()
            .unwrap()
            .get(&run.bot.id)
            .is_some_and(|b| !b.paused)
    });
    // In the same run, the owner answering again and the receipt recovery
    // pass included: nothing makes the dropped re-send owed again.
    quiet();
    assert_eq!(run.submits(), 1, "nothing sent after Resume alone");
    after_a_restart_only_the_persons_retry_sends(run);
}

/// The proof's second case, inverted: the re-send the conversation's own
/// Trash refused is not sent into the restored conversation by a restart.
#[test]
fn a_stale_re_send_its_conversations_trash_refused_stays_dropped_after_restore_and_a_restart() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    refused_for_good(&run);
    // The reply has ended interrupted (an automatic end, not a Stop) before
    // the owner goes silent and the person trashes the conversation.
    until("the reply ended interrupted", || {
        replies(&run.client, run.chat) == vec![MessageState::Interrupted]
    });
    run.port.unavailable.store(true, Ordering::SeqCst);
    // The person trashes the conversation whose message was refused.
    let trash = BotRequestId::mint();
    trash_chat_at(
        &run.client,
        &run.bot,
        run.chat,
        trash,
        read_bot(&run.client, run.bot.id).revision,
    )
    .unwrap();
    run.port.unavailable.store(false, Ordering::SeqCst);
    let done = settled(&run.client, run.bot.id, trash);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    quiet();
    quiet();
    assert_eq!(run.submits(), 1, "the Trash refused the re-send");
    // The person restores it: nothing runs.
    super::trash::restore_chat_at(
        &run.client,
        &run.bot,
        run.chat,
        trash,
        read_bot(&run.client, run.bot.id).revision,
    )
    .unwrap();
    quiet();
    assert_eq!(run.submits(), 1, "nothing ran after the Restore");
    assert_eq!(
        replies(&run.client, run.chat),
        vec![MessageState::Interrupted]
    );
    after_a_restart_only_the_persons_retry_sends(run);
}

/// The proof's third case, inverted: the whole Bot's Archive, Unarchive and
/// Resume, then a restart, send nothing by themselves.
#[test]
fn a_stale_re_send_an_archive_refused_stays_dropped_after_unarchive_resume_and_a_restart() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    until("the Send reached the owner", || run.submits() == 1);
    until("the reply ended interrupted", || {
        replies(&run.client, run.chat) == vec![MessageState::Interrupted]
    });
    run.port.unavailable.store(true, Ordering::SeqCst);
    let current = read_bot(&run.client, run.bot.id);
    let archive_request = BotRequestId::mint();
    archive_at(&run.client, &current, archive_request, current.revision).unwrap();
    run.port.unavailable.store(false, Ordering::SeqCst);
    let done = settled(&run.client, run.bot.id, archive_request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    quiet();
    assert_eq!(run.submits(), 1, "the Archive refused the re-send");
    let archived = read_bot(&run.client, run.bot.id);
    super::trash::restore_at(&run.client, &archived, archive_request, archived.revision).unwrap();
    until("the unarchive's pause is delivered", || {
        read_bot(&run.client, run.bot.id).pending_change.is_none()
    });
    run.client
        .bots(
            context(true),
            BotCommand::Resume {
                request_id: BotRequestId::mint(),
                bot_id: run.bot.id,
                expected_revision: read_bot(&run.client, run.bot.id).revision,
            },
        )
        .unwrap();
    until("the owner holds the Bot resumed", || {
        run.port
            .barrier
            .lock()
            .unwrap()
            .get(&run.bot.id)
            .is_some_and(|b| !b.paused)
    });
    quiet();
    assert_eq!(run.submits(), 1, "nothing ran after Unarchive and Resume");
    after_a_restart_only_the_persons_retry_sends(run);
}

/// A person's Stop of the interrupted reply, in the run that saw the
/// refusal and before its re-send could go (the owner silent): nothing in
/// that run sends it once the owner answers again, and neither does a
/// restart, though the Stop is remembered only in memory.
#[test]
fn a_persons_stop_of_the_refused_reply_declines_its_re_send_in_the_run_and_after_a_restart() {
    let run = sibling(1);
    let (release, attempt) = run.send_held_attempt(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    // Page history cannot be captured for now, so a re-send's preparation
    // fails (and is tried again later) and nothing can be sent before the
    // person's Stop reaches the task.
    run.port.capture_history.store(false, Ordering::SeqCst);
    refused_for_good(&run);
    until("the reply ended interrupted", || {
        super::unadmitted::attempt_status(&run.client, attempt) == AttemptStatus::Interrupted
    });
    run.client.cancel_attempt(context(true), attempt).unwrap();
    run.port.capture_history.store(true, Ordering::SeqCst);
    quiet();
    assert_eq!(run.submits(), 1, "the Stop declined the re-send");
    assert_eq!(
        replies(&run.client, run.chat),
        vec![MessageState::Interrupted]
    );
    after_a_restart_only_the_persons_retry_sends(run);
}

/// The AI worker restarts after the re-send: nothing is sent a second time.
#[test]
fn a_restart_after_the_re_send_sends_nothing_more() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    until("the sibling's reply completed", || {
        replies(&run.client, run.chat).last() == Some(&MessageState::Complete)
    });
    let (repository, port, reader, authority, chat) = run.stop();
    let (worker, _returned) = spawn_with(
        repository,
        authority,
        port.clone(),
        Some(reader as Arc<dyn StoreContextPort>),
    );
    quiet();
    assert_eq!(port.submits.lock().unwrap().len(), 2);
    assert_eq!(
        replies(&worker.client(), chat),
        vec![MessageState::Interrupted, MessageState::Complete]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
