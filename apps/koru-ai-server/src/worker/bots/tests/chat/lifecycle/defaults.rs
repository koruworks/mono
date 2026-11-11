// SPDX-License-Identifier: MPL-2.0
//! Editing a Bot while one of its tasks runs (O23), over the Archive tests'
//! running-task harness: the task keeps what it captured, and its
//! conversation keeps its own settings.
use super::*;

#[test]
fn a_running_task_keeps_what_it_captured_when_its_bot_is_edited() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let native = Native {
        hold_observation: true,
        ..Native::default()
    };
    let (begin_tx, begins) = sync_channel(4);
    *native.begin_reports.lock().unwrap() = Some(begin_tx);
    let port = Arc::new(Archiving {
        native,
        ..Archiving::default()
    });
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: proof(authority.clone(), &bot, chat.clone()),
            request: request(&bot),
            reply,
        })
        .unwrap();
    let attempt = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .accepted
        .attempt_id;
    begins.recv_timeout(Duration::from_secs(10)).unwrap();
    // Every default changes while the task runs.
    let mut config = bot.config.clone();
    config.name = "Renamed mid-task".into();
    config.model.requested_model = "fixture-next".into();
    config.mode = Some("plan".into());
    config.fast = Some(true);
    let (reply, updated) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: proof(authority.clone(), &bot, chat.clone()),
            request: BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                config,
            },
            reply,
        })
        .unwrap();
    let Ok(BotResponse::Bot { bot: edited }) =
        updated.recv_timeout(Duration::from_secs(5)).unwrap()
    else {
        panic!("Update")
    };
    assert_eq!(edited.config.model.requested_model, "fixture-next");
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // The conversation is as it was.
    let after = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            &after.title,
            &after.current_model,
            &after.agent_mode,
            after.agent_fast
        ),
        (
            &chat.title,
            &chat.current_model,
            &chat.agent_mode,
            chat.agent_fast
        )
    );
    // The running task's frozen submission still names what it captured.
    let receipt = repository
        .bot_submission_for_attempt(attempt)
        .unwrap()
        .unwrap();
    let submission = &receipt.intent.submission;
    assert_eq!(
        submission
            .request
            .requested_model
            .as_ref()
            .map(|m| m.as_str()),
        Some("fixture")
    );
    assert_eq!(submission.input.mode, None);
    assert_eq!(submission.input.fast, None);
    assert_eq!(
        repository
            .attempt(attempt)
            .unwrap()
            .unwrap()
            .requested_model,
        "fixture"
    );
}
