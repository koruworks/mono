// SPDX-License-Identifier: MPL-2.0
//! Bot picture staging through the full worker and a fake delegate owner.
use super::*;
use base64::Engine as _;
use std::sync::atomic::AtomicUsize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Native,
    /// Every ingest reply is lost and nothing ever appears in a listing.
    Lost,
    /// The owner refuses the ingest for the execution fence.
    Refused,
}

struct Pictures {
    native: Native,
    script: Script,
    ingests: AtomicUsize,
    lists: AtomicUsize,
    order: Mutex<Vec<&'static str>>,
    ingest_gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    /// Every ingest's session and picture identity, in order.
    ingested: Mutex<Vec<(s::SessionId, s::AttachmentId)>>,
}
impl Pictures {
    fn new(script: Script) -> Self {
        Self::with_native(script, Native::default())
    }
    fn with_native(script: Script, native: Native) -> Self {
        Self {
            native,
            script,
            ingests: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            order: Mutex::new(Vec::new()),
            ingest_gate: Mutex::new(None),
            ingested: Mutex::new(Vec::new()),
        }
    }
}
impl BotDelegatePort for Pictures {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match &command {
            D::Observe { .. } => self.order.lock().unwrap().push("observe"),
            D::BeginTurn { .. } => self.order.lock().unwrap().push("begin"),
            D::IngestAttachment {
                fence,
                attachment_id,
                source,
                data_base64,
            } => {
                assert_eq!(fence.turn, None, "staged before the turn exists");
                assert_eq!(*source, s::AttachmentSource::PublicUpload);
                self.order.lock().unwrap().push("ingest");
                self.ingests.fetch_add(1, Ordering::SeqCst);
                self.ingested
                    .lock()
                    .unwrap()
                    .push((fence.session, *attachment_id));
                if let Some((entered, release)) = self.ingest_gate.lock().unwrap().take() {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .unwrap();
                return match self.script {
                    Script::Native => Ok(BotReply::Attachment {
                        attachment: s::Attachment {
                            id: *attachment_id,
                            session_id: fence.session,
                            kind: s::AttachmentKind::Image,
                            source: *source,
                            media_type: "image/png".into(),
                            byte_count: bytes.len() as u64,
                            dimensions: None,
                            status: s::AttachmentStatus::Pending,
                            delivery_mode: s::DeliveryMode::NativeStructured,
                            delivery: s::DeliveryState::Pending,
                            turn_id: None,
                            preview_available: true,
                            created_at_ms: 0,
                        },
                    }),
                    Script::Lost => Err(koru_delegate_api::Failure::new(
                        "delegate_unavailable",
                        "fixture lost reply",
                        true,
                    )),
                    Script::Refused => Err(koru_delegate_api::Failure::new(
                        "bot_fenced",
                        "fixture fence",
                        false,
                    )),
                };
            }
            D::ListAttachments { .. } => {
                self.lists.fetch_add(1, Ordering::SeqCst);
                return Ok(BotReply::Attachments {
                    attachments: Vec::new(),
                });
            }
            _ => {}
        }
        self.native.execute(command)
    }
}

fn png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

struct Running {
    root: tempfile::TempDir,
    worker: WorkerHandle,
    port: Arc<Pictures>,
    context: CommandContext,
    request: SendTurnRequest,
    authority: Arc<Authority>,
    returned: Receiver<Repository>,
}

fn spawn_on(
    root: &std::path::Path,
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Pictures>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        root.to_owned(),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    (AiWorker::new(config).spawn().unwrap(), returned)
}

impl Running {
    /// Only the AI worker stops and starts again, on the same repository,
    /// attachment folder and owner: the delegate and its session stay.
    /// `before_join` runs once the stopped writer has let go of the stream.
    fn restart(self, stream: &AiResponseStream, before_join: impl FnOnce()) -> Self {
        let Self {
            root,
            worker,
            port,
            context,
            request,
            authority,
            returned,
        } = self;
        worker.shutdown();
        // The writer drops every response stream once its loop has ended;
        // only then can a late provider reply no longer reach it.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match stream.events.recv_timeout(Duration::from_secs(10)) {
                Ok(_) => assert!(Instant::now() < deadline, "the writer never stopped"),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("the stopped writer kept its response stream")
                }
            }
        }
        before_join();
        let _ = worker.join();
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        let (worker, returned) = spawn_on(root.path(), repository, authority.clone(), port.clone());
        Self {
            root,
            worker,
            port,
            context,
            request,
            authority,
            returned,
        }
    }
}

/// One Bot with one stored picture, its Send prepared but not yet sent.
fn start(script: Script) -> Running {
    start_with(Pictures::new(script))
}

fn start_with(port: Pictures) -> Running {
    let mut running = boot_with(port);
    let id = koru_ai_core::ChatAttachmentId::new();
    // The same prepared Bot command context a Send carries.
    crate::worker::attachments::put_attachment(
        &running.worker.client(),
        running.context.clone(),
        running.request.chat_id,
        id,
        koru_ai_api::AttachmentUpload {
            file_name: "picture.png".into(),
            source: koru_ai_core::AttachmentSource::Clipboard,
            bytes: png().into(),
        },
    )
    .unwrap();
    running.request.attachment_ids = vec![id];
    running
}

/// One Bot and its Send, with nothing attached yet.
fn boot(script: Script) -> Running {
    boot_with(Pictures::new(script))
}

fn boot_with(port: Pictures) -> Running {
    let root = tempfile::tempdir().unwrap();
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
    let port = Arc::new(port);
    let (worker, returned) = spawn_on(root.path(), repository, authority.clone(), port.clone());
    let request = request(&bot);
    Running {
        root,
        worker,
        port,
        context,
        authority,
        returned,
        request,
    }
}

fn send(running: &Running) -> AiResponseStream {
    let (reply, response) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::SendTurn {
            context: running.context.clone(),
            request: running.request.clone(),
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
}

fn retry(running: &Running, failed: MessageId) -> AiResponseStream {
    let (reply, response) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::RetryTurn {
            context: running.context.clone(),
            chat: running.request.chat_id,
            request: RetryTurnRequest {
                destination_workspace: running.request.destination_workspace,
                operation_id: Uuid::new_v4(),
                message_id: failed,
                surface: AiSurface::Standalone,
                explicit_refs: Vec::new(),
                reasoning_override: None,
                web_search: false,
                replace_pinned_documents: false,
            },
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
}

/// The terminal event, and whether the images-as-files notice came first.
fn terminal_with_notice(stream: &AiResponseStream) -> (PublicAiEvent, bool) {
    let mut notice = false;
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        notice |= matches!(
            event,
            PublicAiEvent::Notice {
                notice: koru_ai_core::AttemptNotice::ImagesAsFiles,
                ..
            }
        );
        if event.is_terminal() {
            return (event, notice);
        }
    }
}

fn terminal(stream: &AiResponseStream) -> PublicAiEvent {
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        if event.is_terminal() {
            return event;
        }
    }
}

#[test]
fn a_bot_picture_is_ingested_once_between_the_preturn_observe_and_begin() {
    let running = start(Script::Native);
    let stream = send(&running);
    assert!(matches!(terminal(&stream), PublicAiEvent::Completed { .. }));
    let port = &running.port;
    assert_eq!(port.ingests.load(Ordering::SeqCst), 1);
    assert_eq!(port.lists.load(Ordering::SeqCst), 0);
    let order = port.order.lock().unwrap().clone();
    let begin = order.iter().position(|step| *step == "begin").unwrap();
    assert_eq!(&order[..begin + 1], ["observe", "ingest", "begin"]);
    let input = port.native.state.lock().unwrap().input.clone().unwrap();
    assert_eq!(input.attachment_ids.len(), 1);
    assert_eq!(port.native.state.lock().unwrap().begin, 1);
    // An exact replay of the same Send resolves the same attempt and never
    // stages again.
    let replay = send(&running);
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, stream.accepted.attempt_id);
    assert_eq!(port.ingests.load(Ordering::SeqCst), 1);
    assert_eq!(port.native.state.lock().unwrap().begin, 1);
}

#[test]
fn a_lost_ingest_reply_is_listed_never_resent_and_the_picture_goes_as_its_path() {
    let running = start(Script::Lost);
    let stream = send(&running);
    let mut notice = false;
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        notice |= matches!(
            event,
            PublicAiEvent::Notice {
                notice: koru_ai_core::AttemptNotice::ImagesAsFiles,
                ..
            }
        );
        if event.is_terminal() {
            assert!(matches!(event, PublicAiEvent::Completed { .. }));
            break;
        }
    }
    assert!(notice, "the picture went as its path with the notice");
    let port = &running.port;
    assert_eq!(port.ingests.load(Ordering::SeqCst), 1, "never resent");
    assert_eq!(
        port.lists.load(Ordering::SeqCst),
        usize::from(crate::worker::bots::chat::MAX_PICTURE_LISTINGS)
    );
    let input = port.native.state.lock().unwrap().input.clone().unwrap();
    assert!(input.attachment_ids.is_empty());
    assert!(input.text.contains("picture.png"), "named by its path");
}

#[test]
fn a_staging_refusal_selects_the_automatic_interrupt_lane_not_stop() {
    let running = start(Script::Refused);
    let (reports, interrupted) = sync_channel(4);
    *running.port.native.interrupt_reports.lock().unwrap() = Some(reports);
    let stream = send(&running);
    assert!(!matches!(
        terminal(&stream),
        PublicAiEvent::Completed { .. }
    ));
    interrupted.recv_timeout(Duration::from_secs(10)).unwrap();
    let state = running.port.native.state.lock().unwrap();
    assert_eq!(state.begin, 0, "a refused staging never begins");
    assert_eq!(
        state.stop_calls, 0,
        "an automatic failure never selects Stop"
    );
    assert!(state.interrupt_calls > 0);
    assert_eq!(running.port.ingests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_retry_after_the_pictures_were_refused_sends_them_as_paths_and_stages_nothing() {
    let running = start(Script::Native);
    running
        .port
        .native
        .begin_failures
        .lock()
        .unwrap()
        .push_back(koru_delegate_api::Failure::new(
            "bot_input_pictures_refused",
            "The Bot operation was refused.",
            false,
        ));
    let (reports, interrupted) = sync_channel(4);
    *running.port.native.interrupt_reports.lock().unwrap() = Some(reports);
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    // A Retry waits for the refused task to settle, as any Retry does.
    assert!(interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    assert_eq!(running.port.ingests.load(Ordering::SeqCst), 1);
    let refused = running
        .port
        .native
        .state
        .lock()
        .unwrap()
        .input
        .clone()
        .unwrap();
    assert_eq!(refused.attachment_ids.len(), 1, "the first went natively");
    let second = retry(&running, first.accepted.assistant_message_id);
    let (event, notice) = terminal_with_notice(&second);
    assert!(
        matches!(event, PublicAiEvent::Completed { .. }),
        "{event:?}"
    );
    assert!(notice, "the picture went as its path with the notice");
    // The refusal is read back from the failed attempt: nothing is staged
    // into the new Run's session, and the input names the picture's path.
    assert_eq!(running.port.ingests.load(Ordering::SeqCst), 1);
    let state = running.port.native.state.lock().unwrap();
    let input = state.input.clone().unwrap();
    assert!(input.attachment_ids.is_empty());
    assert!(input.text.contains("picture.png"));
    assert_eq!(state.begin, 2);
}

#[test]
fn a_persons_stop_during_an_ingest_takes_the_stop_lane_and_never_begins() {
    let running = start(Script::Native);
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    *running.port.ingest_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let (stops, stopped) = sync_channel(4);
    *running.port.native.stop_reports.lock().unwrap() = Some(stops);
    let stream = send(&running);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // Stop does not wait for the in-flight ingest: it reaches the owner first.
    let (reply, response) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::CancelAttempt {
            context: running.context.clone(),
            attempt: stream.accepted.attempt_id,
            reply,
        })
        .unwrap();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Cancelled
    );
    stopped.recv_timeout(Duration::from_secs(10)).unwrap();
    release.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let state = running.port.native.state.lock().unwrap();
    assert_eq!(state.begin, 0, "a late staging reply never begins");
    assert_eq!(
        state.interrupt_calls, 0,
        "a person's Stop is not an Interrupt"
    );
    assert!(state.stop_calls > 0);
    assert_eq!(running.port.ingests.load(Ordering::SeqCst), 1);
}

/// An absolute path on this platform, in a folder whose name has a space and
/// another script, as a person's folders do.
fn local_path(name: &str) -> String {
    if cfg!(windows) {
        format!("C:\\work folder\\文書\\{name}")
    } else {
        format!("/work folder/文書/{name}")
    }
}

#[test]
fn local_path_references_go_as_the_ordinary_route_sends_them_and_are_never_staged() {
    let mut running = boot(Script::Native);
    let mut expected = Vec::new();
    for (kind, name, reference) in [
        (
            koru_ai_core::AttachmentKind::Folder,
            "project",
            s::PathReferenceKind::Folder,
        ),
        (
            koru_ai_core::AttachmentKind::File,
            "notes.md",
            s::PathReferenceKind::File,
        ),
    ] {
        let id = koru_ai_core::ChatAttachmentId::new();
        crate::worker::attachments::reference_attachment(
            &running.worker.client(),
            running.context.clone(),
            running.request.chat_id,
            koru_ai_wire::ReferenceAttachmentRequest {
                id,
                kind,
                path: local_path(name),
            },
        )
        .unwrap();
        running.request.attachment_ids.push(id);
        expected.push(s::PathReference {
            kind: reference,
            path: local_path(name),
        });
    }
    let stream = send(&running);
    let (event, notice) = terminal_with_notice(&stream);
    assert!(
        matches!(event, PublicAiEvent::Completed { .. }),
        "{event:?}"
    );
    assert!(!notice, "no picture went anywhere");
    let port = &running.port;
    // Nothing named by a reference is read, uploaded or listed.
    assert_eq!(port.ingests.load(Ordering::SeqCst), 0);
    assert_eq!(port.lists.load(Ordering::SeqCst), 0);
    let input = port.native.state.lock().unwrap().input.clone().unwrap();
    let mut references = input.references.clone();
    references.sort_by(|a, b| a.path.cmp(&b.path));
    expected.sort_by(|a, b| a.path.cmp(&b.path));
    // In the input's own field, as the ordinary route sends them, and not
    // repeated in the composed text as copied files are.
    assert_eq!(references, expected);
    assert!(input.attachment_ids.is_empty());
    assert!(!input.text.contains("notes.md"), "{}", input.text);
    // An exact replay of the Send resolves the same attempt and begins nothing.
    let replay = send(&running);
    assert!(replay.accepted.replayed);
    assert_eq!(port.native.state.lock().unwrap().begin, 1);
}

#[test]
fn an_ai_restart_mid_staging_selects_the_automatic_interrupt_and_never_begins() {
    let running = start(Script::Native);
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    *running.port.ingest_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let (reports, interrupted) = sync_channel(64);
    *running.port.native.interrupt_reports.lock().unwrap() = Some(reports);
    let stream = send(&running);
    // The picture's one ingest is in flight on the provider pool...
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // ...when only the AI worker stops. Its reply arrives after the writer
    // has gone, and the restarted writer never resumes staging.
    let running = running.restart(&stream, || release.send(()).unwrap());
    assert!(interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    let state = running.port.native.state.lock().unwrap();
    assert_eq!(
        state.begin, 0,
        "a task whose staging was cut short never begins"
    );
    assert_eq!(state.stop_calls, 0, "an AI restart is not a person's Stop");
    assert!(state.interrupt_calls > 0);
    drop(state);
    assert_eq!(
        running.port.ingests.load(Ordering::SeqCst),
        1,
        "never resent"
    );
    assert_eq!(running.port.lists.load(Ordering::SeqCst), 0);
}

#[test]
fn a_retry_stages_fresh_identities_into_the_new_runs_session_never_the_old_one() {
    let (reports, interrupted) = sync_channel(4);
    let running = start_with(Pictures::with_native(
        Script::Native,
        Native {
            // The first native turn fails: an automatic end a Retry follows.
            fail_first: true,
            interrupt_reports: Mutex::new(Some(reports)),
            ..Native::default()
        },
    ));
    let first = send(&running);
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    assert!(interrupted.recv_timeout(Duration::from_secs(10)).unwrap());
    let old_session = running
        .port
        .native
        .state
        .lock()
        .unwrap()
        .fence
        .as_ref()
        .unwrap()
        .session;
    let second = retry(&running, first.accepted.assistant_message_id);
    let (event, notice) = terminal_with_notice(&second);
    assert!(
        matches!(event, PublicAiEvent::Completed { .. }),
        "{event:?}"
    );
    assert!(!notice, "the Retry's picture went natively");
    let ingested = running.port.ingested.lock().unwrap().clone();
    let state = running.port.native.state.lock().unwrap();
    let new_session = state.fence.as_ref().unwrap().session;
    assert_ne!(old_session, new_session, "a Retry is a new Run and Session");
    // One upload into each Run's own session, each under its own identity.
    assert_eq!(ingested.len(), 2);
    assert_eq!(ingested[0].0, old_session);
    assert_eq!(ingested[1].0, new_session);
    assert_ne!(ingested[0].1, ingested[1].1);
    assert_eq!(
        state.input.as_ref().unwrap().attachment_ids,
        [ingested[1].1]
    );
    assert_eq!(state.begin, 2);
}
