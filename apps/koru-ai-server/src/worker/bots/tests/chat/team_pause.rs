// SPDX-License-Identifier: MPL-2.0
//! M8 piece W, cohort `m8_admission_inventory`: rows 1, 2 and 4 of the team
//! pause's admission inventory on the AI side. A message to an idle Bot
//! (which is also what the external `start_bot_task` sends, and what a retry
//! and an automatic re-send end in) is refused `team_paused`, writes
//! nothing and starts nothing. The steer rows are in `controls.rs`.
use super::*;

fn spawn(native: Arc<Native>) -> (WorkerHandle, CommandContext, Bot) {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context = proof(authority.clone(), &bot, chat);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native);
    config.test_repository = Some(repository);
    (AiWorker::new(config).spawn().unwrap(), context, bot)
}

pub(super) fn team(client: &AiWorkerClient, paused: bool, expected: u64) {
    let request_id = BotRequestId::mint();
    let command = if paused {
        BotCommand::PauseTeam {
            request_id,
            expected_team_revision: expected,
        }
    } else {
        BotCommand::ResumeTeam {
            request_id,
            expected_team_revision: expected,
        }
    };
    assert!(matches!(
        client.bots(super::super::context(true), command),
        Ok(BotResponse::TeamPause { team }) if team.paused == paused
    ));
}

fn send(
    worker: &WorkerHandle,
    context: &CommandContext,
    request: &SendTurnRequest,
) -> ApiResult<AiResponseStream> {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::SendTurn {
            context: context.clone(),
            request: request.clone(),
            reply,
        })
        .unwrap();
    response.recv_timeout(Duration::from_secs(5)).unwrap()
}

#[test]
fn m8_admission_inventory_rows_1_2_and_4_a_message_to_an_idle_bot_is_refused_and_starts_nothing() {
    let native = Arc::new(Native::default());
    let (worker, context, bot) = spawn(native.clone());
    let client = worker.client();
    team(&client, true, 0);

    let request = request(&bot);
    // The same message twice: the second is what a retry of it sends.
    for _ in 0..2 {
        let refused = match send(&worker, &context, &request) {
            Err(refused) => refused,
            Ok(_) => panic!("a message to an idle Bot was accepted under a team pause"),
        };
        assert_eq!(
            (refused.code.as_str(), refused.retryable),
            (koru_ai_wire::bot_team_codes::TEAM_PAUSED, false)
        );
    }
    // Nothing was written and nothing started: no message in the
    // conversation, nothing admitted by the execution owner, no turn begun.
    assert!(
        client
            .read_chat(super::super::context(true), bot.main_chat)
            .unwrap()
            .messages
            .is_empty()
    );
    {
        let state = native.state.lock().unwrap();
        assert!(state.submission.is_none());
        assert_eq!(state.begin, 0);
    }

    // The team resumes: the same message, with the same operation id, runs.
    team(&client, false, 1);
    let stream = send(&worker, &context, &request).unwrap();
    let mut text = String::new();
    loop {
        match stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event
        {
            PublicAiEvent::TextDelta { text: delta, .. } => text.push_str(&delta),
            PublicAiEvent::Completed { .. } => break,
            event if event.is_terminal() => panic!("unexpected terminal: {event:?}"),
            _ => {}
        }
    }
    assert_eq!(text, "NATIVE-REPLY");
    assert_eq!(native.state.lock().unwrap().begin, 1);
    worker.shutdown();
    let _ = worker.join();
}
