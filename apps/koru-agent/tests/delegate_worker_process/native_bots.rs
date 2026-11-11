// SPDX-License-Identifier: MPL-2.0
//! Explicit opt-in process acceptance. The ordinary suite returns before I/O.
use super::*;
use koru_ai_core::bots::{
    BotAccess, BotCommand, BotConfig, BotCreationConfig, BotCreationStage, BotFolderChoice, BotId,
    BotPage, BotRequestId, BotResponse, BotRoleInput,
};
use koru_ai_core::{AiProviderId, AiSurface, AttemptStatus, ModelSelection, ReasoningChoice};
use koru_ai_wire::SendTurnRequest;
#[path = "native_bots/catalog.rs"]
mod catalog;
#[path = "native_bots/stack.rs"]
mod stack;

#[test]
fn scripted_native_bot_with_catalog_fixture_public_chat_when_opted_in() {
    // Run this EXACT test name: the same existing guard also enables real
    // provider tests elsewhere. This path uses only the checked-in fixture.
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let stack = stack::Stack::start();
    let (_, workspace) = workspace(&stack.client);
    let (role, _, _) = a_pinned_page(
        &stack.client,
        workspace,
        "Use native Koru pages for this synthetic fixture.",
    );
    // A window with the workspace open: it lists documents from the
    // manifest room, and must see pages the owner creates in the Store
    // worker without rejoining (the first window run's finding 7).
    let mut window = ManifestWindow::join(&stack.client, workspace);
    let folder = stack.root.path().join("bot-working-folder");
    std::fs::create_dir_all(&folder).unwrap();
    let report = stack.root.path().join("native-result.json");
    let finish = stack.root.path().join("native-finish");
    std::fs::write(
        stack.root.path().join("native-config.json"),
        serde_json::to_vec(&serde_json::json!({
            "workspace": workspace, "report": report, "finish": finish
        }))
        .unwrap(),
    )
    .unwrap();
    let request = BotCommand::Create {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: workspace,
        config: BotConfig {
            name: "Native fixture".into(),
            role: BotPage {
                workspace,
                document: role,
            },
            working_folder: folder.to_string_lossy().into_owned(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture-model".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: Vec::new(),
    };
    let BotResponse::Bot { bot } = stack.client.ai().bots(&request).unwrap() else {
        panic!("configuration reply")
    };
    assert_eq!(bot.owner_device, stack.device);
    assert_eq!((bot.revision, bot.policy_revision), (1, 1));
    // A Bot created with an authored Role: the open window lists the new
    // Role page as soon as the creation reports it, with no restart.
    let role_page = create_with_authored_role(&stack.client, workspace);
    window.wait_for(role_page, "the new Role page in an open window's manifest");
    // One small picture through the public AI attachment API, staged into the
    // Run's session natively before the first input is frozen.
    let picture = koru_ai_core::ChatAttachmentId::new();
    stack
        .client
        .ai()
        .put_attachment(
            bot.main_chat,
            picture,
            "picture.png",
            koru_ai_core::AttachmentSource::Clipboard,
            ONE_PIXEL_PNG.to_vec().into(),
        )
        .unwrap();
    let send = SendTurnRequest {
        destination_workspace: Some(workspace),
        operation_id: uuid::Uuid::new_v4(),
        chat_id: bot.main_chat,
        surface: AiSurface::Standalone,
        markdown: "CURRENT-NATIVE-TASK".into(),
        explicit_refs: Vec::new(),
        reasoning_override: None,
        web_search: false,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: vec![picture],
        answers: None,
        skills: Vec::new(),
    };
    let mut stream = stack.client.ai().send_turn(&send).unwrap();
    let accepted = stream.accepted.clone();
    let replay = stack.client.ai().send_turn(&send).unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);
    assert_eq!(replay.accepted.user_message_id, accepted.user_message_id);
    assert_eq!(
        replay.accepted.assistant_message_id,
        accepted.assistant_message_id
    );
    drop(replay);
    let mut result = None;
    wait_until("native fixture evidence", Duration::from_secs(120), || {
        result = std::fs::read(&report)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if result.is_none() {
            let status = stack
                .client
                .ai()
                .read_attempt(accepted.attempt_id)
                .unwrap()
                .attempt
                .status;
            if status.is_terminal() {
                stack.diagnose_attempt(accepted.attempt_id);
            }
            assert!(
                !status.is_terminal(),
                "native fixture ended before evidence: {status:?}"
            );
        }
        result.is_some()
    });
    let result = result.unwrap();
    assert_eq!(
        result["status"], "ready",
        "fixture reports only a closed stage"
    );
    let ai = koru_ai_sqlite::Repository::open(&stack.root.path().join("ai/ai.db")).unwrap();
    let fence = ai
        .bot_replay_state(accepted.attempt_id)
        .unwrap()
        .fence
        .expect("actual AI attempt association");
    assert_eq!(fence.bot, bot.id);
    assert_eq!(fence.generation.get(), 1);
    let summary = stack
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .find(|row| row.id == fence.session)
        .unwrap();
    assert!(!matches!(
        summary.origin,
        koru_session_protocol::SessionOrigin::Chat { .. }
    ));
    assert!(matches!(
        summary.mode,
        koru_session_protocol::SessionMode::Managed { .. }
    ));
    assert_eq!(
        summary
            .model
            .as_ref()
            .and_then(|model| model.requested.as_ref())
            .map(|model| model.as_str()),
        Some("fixture-model")
    );
    // The turn is still open: the picture was handed to the agent natively,
    // exactly once, from the session's own attachment directory.
    let staged = stack
        .root
        .path()
        .join("agent/sessions/attachments")
        .join(fence.session.to_string());
    assert_eq!(
        std::fs::read_dir(&staged)
            .map(|entries| entries.count())
            .unwrap_or(0),
        1,
        "one staged picture in the Run's session"
    );
    let trace = std::fs::read_to_string(stack.root.path().join("native-fixture.trace")).unwrap();
    assert!(
        trace.lines().any(|line| line == "native:images:1"),
        "the agent was given exactly one native image"
    );
    assert!(trace.lines().any(|line| line == "native:images-on-disk:1"));
    // The Run through the public Bots API (P1.8 B1), found from the public
    // transcript alone, while its session is live: no session handle.
    let (reply, run, running) = read_reply_run(&stack.client, &bot, fence.session);
    assert_eq!((reply, run), (accepted.assistant_message_id, fence.run));
    assert_eq!(running.header.run, fence.run);
    assert_eq!(running.execution.device, stack.device);
    assert_eq!(running.execution.session, None);
    std::fs::write(&finish, []).unwrap();
    let mut terminal = false;
    for frame in &mut stream.events {
        let frame = frame.unwrap();
        if frame.event.is_terminal() {
            terminal = true;
            break;
        }
    }
    assert!(terminal);
    assert_eq!(
        stack
            .client
            .ai()
            .read_attempt(accepted.attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );
    let run_document = stack
        .runs()
        .intent(fence.run)
        .unwrap()
        .unwrap()
        .run_document
        .unwrap();
    let mut header = None;
    wait_until("native delivery and canonical completion", PROMPT, || {
        let bytes = stack
            .client
            .store()
            .snapshot(workspace, run_document)
            .unwrap();
        let current = RunDocument::from_snapshot(&bytes)
            .unwrap()
            .header()
            .unwrap();
        let complete = matches!(current.status, RunStatus::Completed { .. })
            && current.chat_delivery.is_some();
        header = Some(current);
        complete
    });
    let header = header.unwrap();
    assert_eq!(stack.runs().intents().unwrap().len(), 1);
    assert!(header.artifacts.is_empty());
    assert_eq!(header.native_results.len(), 2);
    let created = &header.native_results[0];
    let edited = &header.native_results[1];
    assert_eq!(
        created.kind,
        koru_delegate_core::NativeResultKind::CreatedPage
    );
    assert_eq!(
        edited.kind,
        koru_delegate_core::NativeResultKind::EditedPage
    );
    assert_eq!(created.document, edited.document);
    // The page the Bot's native tool created reached the open window too.
    window.wait_for(
        created.document,
        "the Bot's native page in an open window's manifest",
    );
    assert_ne!(
        loro::VersionVector::decode(&created.version).unwrap(),
        loro::VersionVector::decode(&edited.version).unwrap()
    );
    let receipt = header.chat_delivery.unwrap();
    assert_eq!(receipt.run, fence.run);
    assert_eq!(receipt.ai_attempt_id, accepted.attempt_id.as_uuid());
    assert_eq!(receipt.chat_id, bot.main_chat.as_uuid());
    assert_eq!(
        receipt.assistant_message_id,
        accepted.assistant_message_id.as_uuid()
    );
    assert_eq!(receipt.attempt, fence.attempt);
    assert_eq!(receipt.generation, fence.generation);
    // Read again once finished: the same canonical Run, its whole history.
    let (reply, run, finished) = read_reply_run(&stack.client, &bot, fence.session);
    assert_eq!((reply, run), (accepted.assistant_message_id, fence.run));
    assert_eq!(finished.header.run, fence.run);
    assert!(matches!(
        finished.header.status,
        RunStatus::Completed { .. }
    ));
    assert_eq!(
        finished.header.chat_delivery.as_ref().map(|r| r.run),
        Some(fence.run)
    );
    assert_eq!(finished.header.native_results, header.native_results);
    assert_eq!(finished.execution.session, None);
    assert!(!finished.timeline.items.is_empty());
    assert_eq!(
        finished.timeline.items.len() as u64,
        finished.timeline.total
    );
    let history = stack
        .client
        .store()
        .history(workspace, created.document, &Default::default())
        .unwrap();
    let versions = history
        .entries
        .iter()
        .map(|entry| {
            let snapshot = stack
                .client
                .store()
                .version_snapshot(workspace, entry.version)
                .unwrap()
                .unwrap();
            let doc = loro::LoroDoc::new();
            doc.import(&snapshot.snapshot).unwrap();
            let version = doc.oplog_vv();
            let body = paragraph_text(&snapshot.snapshot);
            (version, body, entry.pinned)
        })
        .collect::<Vec<_>>();
    assert!(versions.contains(&(
        loro::VersionVector::decode(&created.version).unwrap(),
        vec!["NATIVE ORIGINAL".into()],
        true
    )));
    assert!(versions.contains(&(
        loro::VersionVector::decode(&edited.version).unwrap(),
        vec!["NATIVE EDITED".into()],
        true
    )));
    assert_eq!(
        paragraph_text(
            &stack
                .client
                .store()
                .snapshot(workspace, edited.document)
                .unwrap()
        ),
        vec!["NATIVE EDITED"]
    );
    let transcript = stack.client.ai().read_chat(bot.main_chat).unwrap();
    let reply = transcript
        .messages
        .iter()
        .find(|message| message.id == accepted.assistant_message_id)
        .unwrap();
    assert_eq!(reply.markdown, "FIXTURE_NATIVE_TEXT");
    // Content-free shape of the projection: roles, kinds and statuses only.
    for message in &transcript.messages {
        eprintln!(
            "native transcript message: role={:?} state={:?} reply={} blocks={:?}",
            message.role,
            message.state,
            message.id == accepted.assistant_message_id,
            transcript
                .blocks
                .iter()
                .filter(|block| block.message_id == message.id)
                .map(|block| block.kind)
                .collect::<Vec<_>>()
        );
    }
    for activity in &transcript.tool_activities {
        eprintln!(
            "native transcript tool: attempt_match={} reply_match={} status={:?} read_only={} labelled={}",
            activity.attempt_id == accepted.attempt_id,
            activity.assistant_message_id == accepted.assistant_message_id,
            activity.status,
            activity.read_only,
            activity.agent_label.is_some()
        );
    }
    // Native tool calls are projected as durable tool rows, not as message
    // blocks: message blocks hold only the reply's own markdown.
    assert_eq!(
        transcript
            .blocks
            .iter()
            .filter(|block| block.message_id == accepted.assistant_message_id)
            .map(|block| block.kind)
            .collect::<Vec<_>>(),
        [koru_ai_core::MessageBlockKind::Paragraph],
        "the reply's markdown is finalized as its own block"
    );
    let tools = transcript
        .tool_activities
        .iter()
        .filter(|activity| {
            activity.attempt_id == accepted.attempt_id
                && activity.assistant_message_id == accepted.assistant_message_id
        })
        .collect::<Vec<_>>();
    // The scripted turn makes eight native calls: six succeed and two are
    // deliberate refusals (a changed-payload create and a stale edit).
    assert_eq!(tools.len(), 8, "every native call is durably projected");
    assert!(tools.iter().all(|activity| activity.agent_label.is_some()));
    let count = |status| {
        tools
            .iter()
            .filter(|activity| activity.status == status)
            .count()
    };
    assert_eq!(count(koru_ai_core::ToolActivityStatus::Complete), 6);
    assert_eq!(count(koru_ai_core::ToolActivityStatus::Failed), 2);
    wait_until("native authority revoked", PROMPT, || {
        stack
            .client
            .artifacts()
            .run_authority(fence.run)
            .is_ok_and(|authority| {
                authority.state == koru_artifact_wire::authority::AuthorityState::Revoked
            })
    });
    wait_until("scripted provider exited", PROMPT, || {
        stack.sessions.list().is_ok_and(|rows| {
            rows.iter()
                .find(|row| row.id == fence.session)
                .is_none_or(|row| {
                    matches!(
                        row.process,
                        koru_session_protocol::ProcessState::Exited { .. }
                            | koru_session_protocol::ProcessState::Failed { .. }
                    )
                })
        })
    });
    // The session host removes the session's attachment directory once the
    // provider has exited; Koru never removes a staged copy itself.
    wait_until("staged picture removed with the session", PROMPT, || {
        !staged.exists()
    });
    let trace = std::fs::read_to_string(stack.root.path().join("native-fixture.trace")).unwrap();
    assert_eq!(
        trace.lines().filter(|line| *line == "native:turn").count(),
        1
    );
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.starts_with("native:images:"))
            .collect::<Vec<_>>(),
        ["native:images:1"],
        "one turn, one native image"
    );
    assert!(trace.lines().any(|line| line == "native:catalog:4"));
    assert_eq!(
        trace
            .lines()
            .filter_map(|line| line.strip_prefix("native:call:"))
            .collect::<Vec<_>>(),
        [
            "native-create",
            "native-create-replay",
            "native-read-before",
            "native-edit",
            "native-edit-replay",
            "native-create-conflict",
            "native-stale-edit",
            "native-read-after",
        ]
    );
    assert!(!trace.lines().any(|line| line == "native:failed"));
    assert!(
        trace
            .lines()
            .any(|line| line == "native:model:fixture-model")
    );
    assert_eq!(
        names_of(&activity(&stack.client, run_document))
            .iter()
            .filter(|name| **name == "delegate.run.completed")
            .count(),
        1
    );
    stack.verify_catalog();
    // A task started through the public API (P1.8): it writes no reply, so
    // its Run is found through `read_operation` and read without a message,
    // and the API's own Stop ends it as the person's Stop.
    let started = BotRequestId::mint();
    let answer = stack
        .client
        .ai()
        .bots(&BotCommand::Start {
            request_id: started,
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
            destination: workspace,
            task: koru_ai_core::bots::BotTask {
                text: "CURRENT-API-TASK".into(),
                documents: Vec::new(),
            },
        })
        .unwrap();
    assert!(matches!(answer, BotResponse::Operation { .. }));
    let mut task_run = None;
    wait_until("the API task is accepted", PROMPT, || {
        if let BotResponse::Operation { operation } = stack
            .client
            .ai()
            .bots(&BotCommand::ReadOperation {
                request_id: started,
                bot_id: bot.id,
            })
            .unwrap()
            && operation.status == koru_ai_core::bots::BotOperationStatus::Accepted
        {
            task_run = operation.run_id;
        }
        task_run.is_some()
    });
    let task_run = task_run.unwrap();
    let read = stack
        .client
        .ai()
        .bots(&BotCommand::ReadRun {
            bot_id: bot.id,
            chat_id: bot.main_chat,
            message_id: None,
            run_id: task_run,
            from: 0,
            len: koru_ai_core::bots::BOT_RUN_WINDOW_LIMIT,
        })
        .unwrap();
    let BotResponse::Run { view } = read else {
        panic!("the API task's Run view")
    };
    assert_eq!(view.header.run, task_run);
    assert_eq!(view.execution.session, None);
    assert_eq!(
        view.header
            .bot
            .as_ref()
            .map(|bot| (bot.submission, bot.ai_attempt_id)),
        Some((started, None))
    );
    use koru_ai_core::bots::BotTaskStopOutcome as Outcome;
    let stop = || match stack
        .client
        .ai()
        .bots(&BotCommand::StopTask {
            request_id: started,
            bot_id: bot.id,
        })
        .unwrap()
    {
        BotResponse::TaskStop { stop } => stop,
        other => panic!("a task stop, not {other:?}"),
    };
    // The task was just accepted, and the scripted provider takes longer to
    // start and run its turn than this takes: the task is queued or running,
    // so the first Stop either ends it or is delivered with its end not yet
    // proven (what the real owner answered on the first run of this step).
    // Never "already settled".
    let first = stop();
    assert_eq!(first.run_id, task_run);
    eprintln!("API task first stop: {:?}", first.outcome);
    assert!(
        matches!(first.outcome, Outcome::Stopped | Outcome::Stopping),
        "{:?}",
        first.outcome
    );
    // A client asks again, a bounded number of times, while it is stopping.
    let mut last = first.outcome;
    for _ in 0..50 {
        if last != Outcome::Stopping {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        last = stop().outcome;
        eprintln!("API task stop again: {last:?}");
    }
    assert!(
        matches!(last, Outcome::Stopped | Outcome::AlreadySettled),
        "{last:?}"
    );
    // Settled now: one more answers so, and the Run reads as the person's
    // cancellation, never an automatic interruption.
    assert_eq!(stop().outcome, Outcome::AlreadySettled);
    let BotResponse::Run { view } = stack
        .client
        .ai()
        .bots(&BotCommand::ReadRun {
            bot_id: bot.id,
            chat_id: bot.main_chat,
            message_id: None,
            run_id: task_run,
            from: 0,
            len: koru_ai_core::bots::BOT_RUN_WINDOW_LIMIT,
        })
        .unwrap()
    else {
        panic!("the API task's Run view")
    };
    assert_eq!(view.header.status, RunStatus::Cancelled);
    // Its operation still reads as the admitted task it was.
    let BotResponse::Operation { operation } = stack
        .client
        .ai()
        .bots(&BotCommand::ReadOperation {
            request_id: started,
            bot_id: bot.id,
        })
        .unwrap()
    else {
        panic!("the API task's operation")
    };
    assert_eq!(
        operation.status,
        koru_ai_core::bots::BotOperationStatus::Accepted
    );
    assert_eq!(operation.run_id, Some(task_run));
    eprintln!(
        "scripted native Bot verified: four-tool catalog, one turn, durable receipts, delivery, revocation and provider exit; an API task read and stopped"
    );
}

#[test]
fn ordinary_supervised_client_cannot_manage_bots_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let agent = Agent::start(false);
    let client = agent.gateway();
    let (_, workspace) = workspace(&client);
    let ordinary = koru_api_client::Client::connect_at_with_scopes(
        segment(),
        agent.root(),
        &["bots.manage", "ai.chat"],
    )
    .unwrap();
    let failure = ordinary
        .ai()
        .bots(&BotCommand::List { workspace })
        .unwrap_err();
    assert_eq!(failure.code, "forbidden");
    assert!(!failure.retryable);
    eprintln!("ordinary supervised client verified: bots.manage forbidden and nonretryable");
}

/// The one reply of the Bot's main conversation that names its Run in the
/// public transcript, and `read_run` with exactly those two ids, its whole
/// history in one window; the raw answer carries no session.
/// A window's manifest sync session: what the Documents tree lists from.
struct ManifestWindow {
    connection: Connection,
    manifest: koru_store_core::manifest::Manifest,
}

impl ManifestWindow {
    fn join(client: &koru_api_client::Client, workspace: koru_delegate_core::WorkspaceId) -> Self {
        let id = client.store().manifest(workspace).unwrap();
        let snapshot = client.store().snapshot(workspace, id).unwrap();
        let manifest = koru_store_core::manifest::Manifest::from_snapshot(&snapshot).unwrap();
        let mut connection = Connection::open(&client.store().discovery()).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let room = format!("workspace:{}:manifest", workspace.to_storage());
        connection.join(&room, &manifest.version()).unwrap();
        let mut window = Self {
            connection,
            manifest,
        };
        window.drain();
        window
    }

    /// Everything the room has said so far, imported.
    fn drain(&mut self) {
        loop {
            match self.connection.next_event() {
                Ok(Some(Event::Updates { updates, .. })) => {
                    for update in updates {
                        self.manifest.import(&update).unwrap();
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Wait until the room has told this window about `document`. The window
    /// never rejoins or reads a snapshot again: only an announcement can do it.
    fn wait_for(&mut self, document: koru_delegate_core::DocumentId, what: &str) {
        wait_until(what, PROMPT, || {
            self.drain();
            self.manifest
                .entries()
                .iter()
                .any(|entry| entry.id.as_uuid() == document.as_uuid())
        });
    }
}

/// Create a second Bot with an authored Role page and a managed folder, and
/// wait for the creation to complete; answers the Role page.
fn create_with_authored_role(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
) -> koru_delegate_core::DocumentId {
    let request_id = BotRequestId::mint();
    let command = BotCommand::CreateBotWithRole {
        request_id,
        bot_id: BotId::mint(),
        home_workspace: workspace,
        role: BotRoleInput::Authored {
            title: "Fixture role".into(),
            markdown: "A Role page for the open-window check.".into(),
        },
        folder: BotFolderChoice::Managed {},
        config: BotCreationConfig {
            name: "Role fixture".into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture-model".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: Vec::new(),
    };
    client.ai().bots(&command).unwrap();
    let mut role_page = None;
    wait_until("the Bot creation with its Role page", PROMPT, || {
        let BotResponse::Creation { creation } = client
            .ai()
            .bots(&BotCommand::ReadBotCreation { request_id })
            .unwrap()
        else {
            panic!("creation reply")
        };
        assert!(
            matches!(
                creation.stage,
                BotCreationStage::Pending | BotCreationStage::Completed
            ),
            "creation stopped: {:?} {:?}",
            creation.stage,
            creation.reason
        );
        role_page = creation.role_page.map(|page| page.document);
        creation.stage == BotCreationStage::Completed
    });
    role_page.expect("an authored Role page")
}

fn read_reply_run(
    client: &koru_api_client::Client,
    bot: &koru_ai_core::bots::Bot,
    session: koru_delegate_core::SessionId,
) -> (
    koru_ai_core::MessageId,
    koru_delegate_core::RunId,
    koru_delegate_core::RunView,
) {
    let transcript = client.ai().read_chat(bot.main_chat).unwrap();
    let named: Vec<_> = transcript
        .usage
        .iter()
        .filter_map(|entry| entry.run_id.map(|run| (entry.message_id, run)))
        .collect();
    let [(reply, run)] = named[..] else {
        panic!("one reply names its Run: {named:?}")
    };
    let response = client
        .ai()
        .bots(&BotCommand::ReadRun {
            bot_id: bot.id,
            chat_id: bot.main_chat,
            message_id: Some(reply),
            run_id: run,
            from: 0,
            len: koru_ai_core::bots::BOT_RUN_WINDOW_LIMIT,
        })
        .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["result"], "run");
    assert!(wire["view"]["execution"].get("session").is_none());
    // Nothing anywhere in the answer names the Run's session: not the
    // execution state, not the owner's Started step, not an agent actor.
    let text = wire.to_string();
    assert!(
        !text.contains(&session.to_string()),
        "the Run's session leaked through read_run"
    );
    let BotResponse::Run { view } = response else {
        panic!("a Run view")
    };
    (reply, run, view)
}

/// A valid 1x1 RGBA PNG: the smallest picture both the AI's attachment check
/// and the session host's image ingest accept.
const ONE_PIXEL_PNG: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn paragraph_text(snapshot: &[u8]) -> Vec<String> {
    let document = koru_core::WorkspaceDoc::from_snapshot(snapshot).unwrap();
    document
        .walk_all()
        .unwrap()
        .into_iter()
        .filter_map(|(id, _)| {
            (document.read(id).unwrap().kind == koru_core::Kind::Paragraph).then(|| {
                document
                    .runs(id)
                    .unwrap()
                    .into_iter()
                    .map(|run| run.text)
                    .collect()
            })
        })
        .collect()
}
