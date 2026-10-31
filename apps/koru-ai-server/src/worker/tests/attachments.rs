// SPDX-License-Identifier: MPL-2.0

//! Chat attachments through the worker, on a real `ai.db` in a temp dir.

use koru_ai_api::AttachmentUpload;
use koru_ai_core::{AttachmentKind, AttachmentSource, ChatAttachmentId};
use koru_ai_wire::attachment_codes;

use super::*;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

fn upload(name: &str, bytes: Vec<u8>) -> AttachmentUpload {
    AttachmentUpload {
        file_name: name.into(),
        source: AttachmentSource::Clipboard,
        bytes: bytes.into(),
    }
}

/// A worker with a capturing `agent-codex` provider beside the mock one.
fn agent_worker(
    directory: &std::path::Path,
) -> (WorkerHandle, std::sync::mpsc::Receiver<AiRequest>) {
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    (worker, requests)
}

fn create_agent_chat(client: &AiWorkerClient) -> ChatId {
    let chat = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: chat,
                title: "Agent chat".into(),
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse("agent-codex").unwrap(),
                    requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    chat
}

fn send_with(
    client: &AiWorkerClient,
    chat_id: ChatId,
    markdown: &str,
    attachment_ids: Vec<ChatAttachmentId>,
) -> ApiResult<AiResponseStream> {
    client.send_turn(
        context(),
        SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: markdown.into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids,
        },
    )
}

fn refusal(result: ApiResult<AiResponseStream>) -> koru_ai_wire::Failure {
    match result {
        Ok(_) => panic!("the send was accepted"),
        Err(failure) => failure,
    }
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "condition never held");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn chat_folder(directory: &std::path::Path, chat: ChatId) -> std::path::PathBuf {
    directory.join("attachments").join(chat.to_storage())
}

#[test]
fn a_pasted_picture_is_stored_bound_and_carried_to_the_agent() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let id = ChatAttachmentId::new();

    let stored = client
        .put_attachment(context(), chat, id, upload("screenshot.png", png(40, 30)))
        .unwrap()
        .attachment;
    assert_eq!(stored.kind, AttachmentKind::Image);
    assert_eq!(stored.dimensions, Some((40, 30)));
    assert_eq!(stored.message_id, None);
    let staged = client.read_chat(context(), chat).unwrap().attachments;
    assert_eq!(staged, vec![stored.clone()]);
    let thumbnail = client
        .read_attachment_thumbnail(context(), chat, id)
        .unwrap();
    assert!(thumbnail.starts_with(b"\x89PNG"));

    let stream = send_with(&client, chat, "What is this?", vec![id]).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let sent = &request.messages.last().unwrap().attachments;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, id);
    assert!(sent[0].path.is_file());
    assert!(sent[0].path.starts_with(directory.path()));
    assert_eq!(
        sent[0].native.as_deref(),
        Some(sent[0].path.as_path()),
        "a small picture goes as it is"
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat).unwrap();
    assert_eq!(
        transcript.attachments[0].message_id,
        Some(stream.accepted.user_message_id)
    );
    assert_eq!(transcript.attachments[0].ordinal, Some(0));
    let original = client.read_attachment(context(), chat, id).unwrap();
    assert_eq!(original.file_name, "screenshot.png");
    assert_eq!(original.media_type, "image/png");

    // The next turn carries the earlier picture in its history.
    let next = send_with(&client, chat, "And now?", Vec::new()).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.messages[0].attachments.len(), 1);
    assert!(request.messages.last().unwrap().attachments.is_empty());
    wait_for_terminal(&client, next.accepted.attempt_id);
}

#[test]
fn an_upload_is_idempotent_by_id_and_a_conflict_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, _requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let id = ChatAttachmentId::new();
    let first = client
        .put_attachment(context(), chat, id, upload("notes.txt", b"one".to_vec()))
        .unwrap();
    let again = client
        .put_attachment(context(), chat, id, upload("notes.txt", b"one".to_vec()))
        .unwrap();
    assert_eq!(first, again);
    let conflict = client
        .put_attachment(context(), chat, id, upload("notes.txt", b"two".to_vec()))
        .unwrap_err();
    assert_eq!(conflict.code, attachment_codes::CONFLICT);
    let other = create_agent_chat(&client);
    assert_eq!(
        client
            .put_attachment(context(), other, id, upload("notes.txt", b"one".to_vec()))
            .unwrap_err()
            .code,
        attachment_codes::CONFLICT,
        "an id is one chat's"
    );
}

#[test]
fn only_an_agent_chat_takes_attachments() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, _requests) = agent_worker(directory.path());
    let client = worker.client();
    let plain = create_chat(&client);
    let refused = client
        .put_attachment(
            context(),
            plain,
            ChatAttachmentId::new(),
            upload("shot.png", png(4, 4)),
        )
        .unwrap_err();
    assert_eq!(refused.code, attachment_codes::UNSUPPORTED);
    assert!(!chat_folder(directory.path(), plain).exists());
    let refused = refusal(send_with(
        &client,
        plain,
        "Hi",
        vec![ChatAttachmentId::new()],
    ));
    assert_eq!(refused.code, attachment_codes::UNSUPPORTED);
    assert!(
        client
            .read_chat(context(), plain)
            .unwrap()
            .messages
            .is_empty()
    );
}

#[test]
fn a_send_with_bad_attachments_writes_no_message() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let other = create_agent_chat(&client);
    let foreign = ChatAttachmentId::new();
    client
        .put_attachment(context(), other, foreign, upload("a.txt", b"a".to_vec()))
        .unwrap();

    let unknown = refusal(send_with(
        &client,
        chat,
        "Hi",
        vec![ChatAttachmentId::new()],
    ));
    assert_eq!(unknown.code, attachment_codes::NOT_FOUND);
    let foreign = refusal(send_with(&client, chat, "Hi", vec![foreign]));
    assert_eq!(foreign.code, attachment_codes::NOT_FOUND);

    let mut eleven = Vec::new();
    for index in 0..11 {
        let id = ChatAttachmentId::new();
        client
            .put_attachment(
                context(),
                chat,
                id,
                upload(&format!("n{index}.txt"), vec![b'x'; 8]),
            )
            .unwrap();
        eleven.push(id);
    }
    let limit = refusal(send_with(&client, chat, "Hi", eleven.clone()));
    assert_eq!(limit.code, attachment_codes::LIMIT);
    let twice = refusal(send_with(&client, chat, "Hi", vec![eleven[0], eleven[0]]));
    assert_eq!(twice.code, "invalid_request");

    // Three 18 MiB files are within the per-file limit but not the message's.
    let mut heavy = Vec::new();
    for index in 0..3 {
        let id = ChatAttachmentId::new();
        client
            .put_attachment(
                context(),
                chat,
                id,
                upload(&format!("big{index}.txt"), vec![b'a'; 18 * 1024 * 1024]),
            )
            .unwrap();
        heavy.push(id);
    }
    let over = refusal(send_with(&client, chat, "Hi", heavy));
    assert_eq!(over.code, attachment_codes::LIMIT);

    // The stored file gone from disk: refused before any row.
    let missing = eleven[1];
    let record_dir = chat_folder(directory.path(), chat).join(missing.to_storage());
    std::fs::remove_dir_all(&record_dir).unwrap();
    let refused = refusal(send_with(&client, chat, "Hi", vec![missing]));
    assert_eq!(refused.code, attachment_codes::MISSING);
    assert_eq!(refused.details["attachment_id"], missing.to_storage());

    assert!(
        client
            .read_chat(context(), chat)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(requests.try_recv().is_err(), "no provider was called");

    // A sent attachment cannot go twice, and stays when asked to go.
    let stream = send_with(&client, chat, "Hi", vec![eleven[2]]).unwrap();
    wait_for_terminal(&client, stream.accepted.attempt_id);
    let resent = refusal(send_with(&client, chat, "Again", vec![eleven[2]]));
    assert_eq!(resent.code, attachment_codes::SENT);
    let kept = client
        .remove_attachment(context(), chat, eleven[2])
        .unwrap_err();
    assert_eq!(kept.code, attachment_codes::SENT);
}

#[test]
fn removing_a_staged_attachment_removes_its_folder() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, _requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let id = ChatAttachmentId::new();
    client
        .put_attachment(context(), chat, id, upload("draft.txt", b"draft".to_vec()))
        .unwrap();
    let folder = chat_folder(directory.path(), chat).join(id.to_storage());
    assert!(folder.is_dir());
    assert!(
        client
            .remove_attachment(context(), chat, id)
            .unwrap()
            .removed
    );
    assert!(!folder.exists());
    assert!(
        !client
            .remove_attachment(context(), chat, id)
            .unwrap()
            .removed
    );
    assert!(
        client
            .read_chat(context(), chat)
            .unwrap()
            .attachments
            .is_empty()
    );
}

#[test]
fn deleting_a_chat_removes_its_rows_and_then_its_folder() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, _requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    client
        .put_attachment(
            context(),
            chat,
            ChatAttachmentId::new(),
            upload("a.txt", b"a".to_vec()),
        )
        .unwrap();
    assert!(chat_folder(directory.path(), chat).is_dir());
    assert!(client.delete_chat(context(), chat).unwrap().deleted);
    wait_until(|| !chat_folder(directory.path(), chat).exists());
}

#[test]
fn a_branch_carries_its_attachments_under_new_ids_in_its_own_folder() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let id = ChatAttachmentId::new();
    client
        .put_attachment(context(), chat, id, upload("shot.png", png(8, 8)))
        .unwrap();
    let stream = send_with(&client, chat, "Look", vec![id]).unwrap();
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_for_terminal(&client, stream.accepted.attempt_id);

    let branch = ChatId::new();
    let transcript = client
        .branch_chat(
            context(),
            chat,
            BranchChatRequest {
                branch_chat_id: branch,
                through_message_id: stream.accepted.assistant_message_id,
                title: "Branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(transcript.attachments.len(), 1);
    let copy = &transcript.attachments[0];
    assert_ne!(copy.id, id);
    assert_eq!(copy.file_name, "shot.png");
    assert_eq!(copy.message_id, Some(transcript.messages[0].id));
    assert!(
        chat_folder(directory.path(), branch)
            .join(copy.id.to_storage())
            .join("file")
            .join("shot.png")
            .is_file()
    );

    // Deleting the source leaves the branch's copy readable.
    client.delete_chat(context(), chat).unwrap();
    wait_until(|| !chat_folder(directory.path(), chat).exists());
    assert_eq!(
        client
            .read_attachment(context(), branch, copy.id)
            .unwrap()
            .file_name,
        "shot.png"
    );
}

#[test]
fn the_start_up_sweep_removes_what_no_row_names() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("attachments");
    let orphan_chat = root.join(ChatId::new().to_storage());
    let staging = root.join(".staging").join("leftover");
    let foreign = root.join("not-an-id");
    for folder in [&orphan_chat, &staging, &foreign] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::thread::sleep(Duration::from_millis(20));
    let (_worker, _requests) = agent_worker(directory.path());
    wait_until(|| !orphan_chat.exists() && !staging.exists());
    assert!(foreign.is_dir());
}

#[test]
fn a_message_may_be_attachments_only_but_never_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, requests) = agent_worker(directory.path());
    let client = worker.client();
    let chat = create_agent_chat(&client);
    let empty = refusal(send_with(&client, chat, "  ", Vec::new()));
    assert_eq!(empty.code, "invalid_request");

    let id = ChatAttachmentId::new();
    client
        .put_attachment(context(), chat, id, upload("shot.png", png(4, 4)))
        .unwrap();
    let stream = send_with(&client, chat, "", vec![id]).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let last = request.messages.last().unwrap();
    assert!(last.markdown.is_empty());
    assert_eq!(last.attachments.len(), 1);
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
}
