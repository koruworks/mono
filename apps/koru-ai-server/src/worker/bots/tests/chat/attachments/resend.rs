// SPDX-License-Identifier: MPL-2.0
//! When the runtime refuses a task's pictures, the AI side sends the task
//! again by itself, once, with the pictures as files (O30), through the
//! person's own Retry path: real preparation, the guard and its holds, and
//! `retry_turn`'s durable records. The fake owner applies the delegate's
//! rules: a Begin's named refusal is final, an Interrupt settles the Run
//! unless held, and a Retry's Submit is admitted only once the refused Run
//! reconciles settled.
use super::*;

/// The test authority, with Store history capture: real preparation of a
/// Send or Retry reads each page's current version through it.
pub(super) struct Capturing(pub(super) Arc<Authority>);
impl BotAuthority for Capturing {
    fn replay_memory_index(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        home: WorkspaceId,
        capture: &BotLibraryCapture,
        binding: Option<&BotMemoryBinding>,
        cancellation: &Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        self.0
            .replay_memory_index(owner, reader, home, capture, binding, cancellation)
    }
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.0.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.0.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.0.page(page)
    }
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: koru_ai_core::ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        Ok(pages
            .iter()
            .map(|page| d::RunContextItem::Document {
                workspace: page.workspace,
                document: page.document,
                version: d::DocumentVersionId::new(),
            })
            .collect())
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<koru_ai_core::bots::BotAuthoritySnapshot, BotError> {
        self.0.inspect(workspaces, pages)
    }
}

fn refusal(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation was refused.", false)
}

/// One Bot with one stored picture whose Begins answer `answers` in order.
fn refusing(answers: &[koru_delegate_api::Failure], native: Native) -> Running {
    let running = start_with(Pictures::with_native(Script::Native, native));
    running
        .port
        .native
        .begin_failures
        .lock()
        .unwrap()
        .extend(answers.iter().cloned());
    running
}

fn begins(running: &Running) -> usize {
    running.port.native.state.lock().unwrap().begin
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Long enough for a re-send that should not happen to have begun: each
/// step of one is a few writer passes and owner calls, with no backoff.
fn quiet() {
    std::thread::sleep(Duration::from_millis(1500));
}

fn stop(running: &Running, attempt: InferenceAttemptId) {
    let (reply, answer) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::CancelAttempt {
            context: running.context.clone(),
            attempt,
            reply,
        })
        .unwrap();
    answer
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
}

fn finish(running: Running) -> Repository {
    running.worker.shutdown();
    let _ = running.worker.join();
    running
        .returned
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
}

/// The conversation's attempts, oldest first.
fn attempts(repository: &Repository, chat: ChatId) -> Vec<InferenceAttempt> {
    let mut attempts = repository.attempts_for_chat(chat).unwrap();
    attempts.sort_by_key(|a| a.queued_at.millis());
    attempts
}

#[test]
fn refused_pictures_are_sent_again_once_as_files_without_a_retry() {
    let running = refusing(&[refusal("bot_input_pictures_refused")], Native::default());
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    // Nobody presses Retry: the task is sent again on its own.
    wait_for("the automatic re-send began", || begins(&running) == 2);
    let input = running
        .port
        .native
        .state
        .lock()
        .unwrap()
        .input
        .clone()
        .unwrap();
    assert!(
        input.attachment_ids.is_empty(),
        "nothing staged the second time"
    );
    assert!(input.text.contains("picture.png"), "named by its path");
    assert_eq!(running.port.ingests.load(Ordering::SeqCst), 1);
    let chat = running.request.chat_id;
    let repository = finish(running);
    let [refused, resent] = attempts(&repository, chat).try_into().unwrap();
    assert_eq!(refused.id, first.accepted.attempt_id);
    assert_eq!(refused.status, AttemptStatus::Failed);
    assert_eq!(
        refused.stop_reason.as_deref(),
        Some("bot_input_pictures_refused")
    );
    assert_eq!(resent.status, AttemptStatus::Complete);
    // A Retry's own records: its reply names the refused reply, and its
    // task continues the refused Run.
    let reply = repository
        .message(resent.assistant_message_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        reply.origin_message_id,
        Some(first.accepted.assistant_message_id)
    );
    let original = repository
        .bot_submission_for_attempt(refused.id)
        .unwrap()
        .unwrap();
    let again = repository
        .bot_submission_for_attempt(resent.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        again.intent.submission.provenance.continued_from,
        Some(original.intent.submission.request.run)
    );
    assert_eq!(
        again.intent.submission.input.text,
        original.intent.submission.input.text
    );
    assert_ne!(
        again.intent.submission.request.run,
        original.intent.submission.request.run
    );
}

#[test]
fn a_second_refusal_fails_visibly_and_nothing_more_is_tried() {
    let (reports, interrupted) = sync_channel(8);
    let native = Native {
        interrupt_reports: Mutex::new(Some(reports)),
        ..Native::default()
    };
    let running = refusing(
        &[
            refusal("bot_input_too_large"),
            refusal("bot_input_too_large"),
        ],
        native,
    );
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    // Both refused tasks end on the automatic lane.
    assert!(interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    assert!(interrupted.recv_timeout(Duration::from_secs(20)).unwrap());
    quiet();
    assert_eq!(begins(&running), 2, "exactly once per task");
    let chat = running.request.chat_id;
    let repository = finish(running);
    let [_, resent] = attempts(&repository, chat).try_into().unwrap();
    assert_eq!(resent.status, AttemptStatus::Failed);
    assert_eq!(resent.stop_reason.as_deref(), Some("bot_input_too_large"));
    let reply = repository
        .message(resent.assistant_message_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(reply.state, MessageState::Failed);
}

#[test]
fn a_failure_that_is_not_a_named_picture_refusal_is_never_sent_again() {
    for answer in [
        // Not one of the owner's named refusals of the input.
        refusal("bot_fenced"),
        // A named refusal that paths cannot get past.
        refusal("bot_input_model_refused"),
    ] {
        let (reports, interrupted) = sync_channel(8);
        let native = Native {
            interrupt_reports: Mutex::new(Some(reports)),
            ..Native::default()
        };
        let running = refusing(&[answer.clone()], native);
        let first = send(&running);
        assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
        assert!(interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
        quiet();
        assert_eq!(begins(&running), 1, "{}", answer.code);
        let chat = running.request.chat_id;
        assert_eq!(attempts(&finish(running), chat).len(), 1);
    }
}

#[test]
fn the_re_send_waits_until_the_refused_run_is_settled() {
    let (reports, interrupted) = sync_channel(64);
    let native = Native {
        interrupt_reports: Mutex::new(Some(reports)),
        hold_interrupt: AtomicBool::new(true),
        ..Native::default()
    };
    let running = refusing(&[refusal("bot_input_pictures_refused")], native);
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    // The owner has not settled the refused Run: nothing is sent.
    assert!(!interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    quiet();
    assert_eq!(begins(&running), 1);
    // Once it settles, the task goes again.
    running
        .port
        .native
        .hold_interrupt
        .store(false, Ordering::SeqCst);
    wait_for("the re-send after settling", || begins(&running) == 2);
    let input = running
        .port
        .native
        .state
        .lock()
        .unwrap()
        .input
        .clone()
        .unwrap();
    assert!(input.attachment_ids.is_empty());
}

#[test]
fn a_persons_stop_of_the_refused_task_cancels_its_re_send() {
    let (reports, interrupted) = sync_channel(64);
    let (stops, stopped) = sync_channel(8);
    let native = Native {
        interrupt_reports: Mutex::new(Some(reports)),
        stop_reports: Mutex::new(Some(stops)),
        hold_interrupt: AtomicBool::new(true),
        // The owner applies the person's Stop.
        fail_first: true,
        ..Native::default()
    };
    let running = refusing(&[refusal("bot_input_pictures_refused")], native);
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    assert!(!interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    stop(&running, first.accepted.attempt_id);
    stopped.recv_timeout(Duration::from_secs(10)).unwrap();
    running
        .port
        .native
        .hold_interrupt
        .store(false, Ordering::SeqCst);
    quiet();
    assert_eq!(begins(&running), 1, "the Stop cancelled the re-send");
    let chat = running.request.chat_id;
    assert_eq!(attempts(&finish(running), chat).len(), 1);
}

/// A re-send is owed only in the worker run that saw the refusal (the fourth
/// lifecycle review's F1): a restart before it sends nothing by itself, the
/// refused reply stays failed, and the person's Retry sends the pictures as
/// files.
#[test]
fn a_restart_before_the_re_send_sends_nothing_and_the_persons_retry_does() {
    let (reports, interrupted) = sync_channel(64);
    let native = Native {
        interrupt_reports: Mutex::new(Some(reports)),
        hold_interrupt: AtomicBool::new(true),
        ..Native::default()
    };
    let running = refusing(&[refusal("bot_input_pictures_refused")], native);
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    assert!(!interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    // Only the AI worker restarts, before anything was sent again.
    let running = running.restart(&first, || {});
    running
        .port
        .native
        .hold_interrupt
        .store(false, Ordering::SeqCst);
    // The restart settles the refused Run on its automatic lane (the held
    // answers before it may still be queued), and sends nothing again.
    while !interrupted.recv_timeout(Duration::from_secs(20)).unwrap() {}
    quiet();
    assert_eq!(begins(&running), 1, "a restart sent nothing by itself");
    let chat = running.request.chat_id;
    // The person's Retry sends it, with the pictures as files.
    let again = super::retry(&running, first.accepted.assistant_message_id);
    let (event, notice) = super::terminal_with_notice(&again);
    assert!(
        matches!(event, PublicAiEvent::Completed { .. }),
        "{event:?}"
    );
    assert!(notice, "images sent as files");
    assert_eq!(begins(&running), 2);
    let all = attempts(&finish(running), chat);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].status, AttemptStatus::Failed);
    assert_eq!(all[1].status, AttemptStatus::Complete);
}

/// A restart after the re-send finds the newer reply and sends nothing.
#[test]
fn a_restart_after_the_re_send_never_sends_a_second() {
    let running = refusing(&[refusal("bot_input_pictures_refused")], Native::default());
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    wait_for("the automatic re-send began", || begins(&running) == 2);
    wait_for("the re-send completed", || {
        running.port.native.state.lock().unwrap().delivered
    });
    let chat = running.request.chat_id;
    let running = running.restart(&first, || {});
    quiet();
    assert_eq!(begins(&running), 2, "never a second re-send");
    let all = attempts(&finish(running), chat);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].status, AttemptStatus::Complete);
}

#[test]
fn a_conversation_in_trash_refuses_the_re_send_like_any_new_work() {
    let (reports, interrupted) = sync_channel(64);
    let native = Native {
        interrupt_reports: Mutex::new(Some(reports)),
        hold_interrupt: AtomicBool::new(true),
        ..Native::default()
    };
    let running = refusing(&[refusal("bot_input_pictures_refused")], native);
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    assert!(!interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    // The conversation goes to Trash while the refused Run settles.
    let Running {
        root,
        worker,
        port,
        context,
        request,
        authority,
        returned,
    } = running;
    worker.shutdown();
    let _ = worker.join();
    let mut repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let bot_id = repository.bot_for_chat(request.chat_id).unwrap().unwrap();
    let bot = repository.bot(bot_id).unwrap().unwrap();
    let operation = repository
        .capture_bot_lifecycle(&super::super::super::holds::capture(
            &bot,
            BotLifecycleKind::TrashChat,
            Some(request.chat_id),
        ))
        .unwrap();
    repository
        .fixture_trash_bot_chat(&operation, request.chat_id, 9)
        .unwrap();
    repository
        .fixture_complete_bot_lifecycle(&operation, 10)
        .unwrap();
    let (worker, returned) = spawn_on(root.path(), repository, authority.clone(), port.clone());
    let running = Running {
        root,
        worker,
        port,
        context,
        request,
        authority,
        returned,
    };
    running
        .port
        .native
        .hold_interrupt
        .store(false, Ordering::SeqCst);
    assert!(interrupted.recv_timeout(Duration::from_secs(20)).unwrap());
    quiet();
    assert_eq!(begins(&running), 1, "the hold refused the re-send");
    let chat = running.request.chat_id;
    assert_eq!(attempts(&finish(running), chat).len(), 1);
}
