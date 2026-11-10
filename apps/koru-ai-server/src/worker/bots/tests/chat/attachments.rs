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
}
impl Pictures {
    fn new(script: Script) -> Self {
        Self {
            native: Native::default(),
            script,
            ingests: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            order: Mutex::new(Vec::new()),
            ingest_gate: Mutex::new(None),
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
    _root: tempfile::TempDir,
    worker: WorkerHandle,
    port: Arc<Pictures>,
    context: CommandContext,
    request: SendTurnRequest,
}

/// One Bot with one stored picture, its Send prepared but not yet sent.
fn start(script: Script) -> Running {
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
    let port = Arc::new(Pictures::new(script));
    let mut config = WorkerConfig::new(
        root.path().to_owned(),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(port.clone());
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let id = koru_ai_core::ChatAttachmentId::new();
    // The same prepared Bot command context a Send carries.
    crate::worker::attachments::put_attachment(
        &worker.client(),
        context.clone(),
        bot.main_chat,
        id,
        koru_ai_api::AttachmentUpload {
            file_name: "picture.png".into(),
            source: koru_ai_core::AttachmentSource::Clipboard,
            bytes: png().into(),
        },
    )
    .unwrap();
    let mut request = request(&bot);
    request.attachment_ids = vec![id];
    Running {
        _root: root,
        worker,
        port,
        context,
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
