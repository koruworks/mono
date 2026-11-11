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
        let release = hold_context_thread(&self.client, &self.reader);
        self.send_in(self.chat);
        if let Some(n) = other {
            self.send_in(self.others[n]);
        }
        release
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

/// The AI worker restarts after the refusal and before the re-send: the new
/// worker sends it again, once.
#[test]
fn a_restart_before_the_re_send_sends_it_once() {
    let run = sibling(1);
    let release = run.send_held(Some(0));
    run.trash_other(0);
    release.send(()).unwrap();
    // The owner goes silent the moment the refusal is final, so the re-send
    // cannot be asked for before the restart.
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
    run.port.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(run.submits(), 1, "nothing was sent again yet");
    let (repository, port, reader, authority, chat) = run.stop();
    port.unavailable.store(false, Ordering::SeqCst);
    let (worker, _returned) = spawn_with(
        repository,
        authority,
        port.clone(),
        Some(reader as Arc<dyn StoreContextPort>),
    );
    let client = worker.client();
    until("the re-sent reply completed", || {
        replies(&client, chat).last() == Some(&MessageState::Complete)
    });
    quiet();
    assert_eq!(port.submits.lock().unwrap().len(), 2);
    assert_eq!(
        replies(&client, chat),
        vec![MessageState::Interrupted, MessageState::Complete]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
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
