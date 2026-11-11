// SPDX-License-Identifier: MPL-2.0
//! Branching a Bot conversation whose prefix holds attached files. These
//! copy real folders, so each test owns a fresh temporary AI root.
use super::*;
use crate::attachments::{Cleanup, CleanupJob};
use crate::worker::bots::branch::settle_uncommitted;
use koru_ai_core::{AttachmentKind, AttachmentRecord, AttachmentSource};
use std::path::Path;

/// A Bot whose finished reply follows a message carrying a stored picture
/// (its file on disk) and a local path reference (a row only).
struct Attached {
    root: tempfile::TempDir,
    repo: Repository,
    authority: Arc<Authority>,
    bot: Bot,
    through: MessageId,
    picture: AttachmentRecord,
    reference: AttachmentRecord,
}

fn attached() -> Attached {
    let root = tempfile::tempdir().unwrap();
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let picture = branch_attachment(bot.main_chat);
    let original = folder(root.path(), bot.main_chat, picture.attachment.id).join("file");
    std::fs::create_dir_all(&original).unwrap();
    std::fs::write(original.join("fixture.png"), b"source picture").unwrap();
    let path = if cfg!(windows) {
        "C:\\work folder\\notes.md"
    } else {
        "/work folder/notes.md"
    };
    let reference = AttachmentRecord {
        chat_id: bot.main_chat,
        attachment: koru_ai_core::ChatAttachment {
            id: ChatAttachmentId::new(),
            kind: AttachmentKind::File,
            source: AttachmentSource::Path,
            file_name: "notes.md".into(),
            media_type: "text/markdown".into(),
            byte_count: 0,
            dimensions: None,
            reference_path: Some(path.into()),
            ..picture.attachment.clone()
        },
        has_sent_copy: false,
        content_blake3: blake3::hash(path.as_bytes()).to_hex().to_string(),
    };
    repo.stage_attachment(&picture).unwrap();
    repo.stage_attachment(&reference).unwrap();
    let pending = complete(
        &mut repo,
        bot.main_chat,
        1,
        vec![picture.attachment.id, reference.attachment.id],
    );
    Attached {
        root,
        repo,
        authority,
        bot,
        through: pending.assistant_message.id,
        picture,
        reference,
    }
}

fn folder(root: &Path, chat: ChatId, id: ChatAttachmentId) -> PathBuf {
    root.join("attachments")
        .join(chat.to_storage())
        .join(id.to_storage())
}

fn chat_folder_entries(root: &Path, chat: ChatId) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(root.join("attachments").join(chat.to_storage()))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "condition never held");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The proof a prepared Branch carries, so the first fresh inspection the
/// worker makes is the one after the files are copied.
fn prepared(authority: &Arc<Authority>, bot: &Bot, destination: ChatId) -> CommandContext {
    CommandContext {
        request: context(true),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: authority.clone(),
            chats: [bot.main_chat, destination]
                .into_iter()
                .map(|chat| (bot.id, chat, bot.revision, bot.policy_revision))
                .collect(),
            runtime: Ok(()),
            pins: vec![],
            chat_snapshot: None,
            settings: None,
            terminal_question: None,
        })),
    }
}

fn send_branch(
    client: &AiWorkerClient,
    context: CommandContext,
    source: ChatId,
    request: BranchChatRequest,
) -> Receiver<ApiResult<BranchChatResponse>> {
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::BranchChat {
            context,
            chat: source,
            request,
            reply,
        })
        .unwrap();
    response
}

/// Hold the next fresh owner inspection: the one a copied branch makes.
fn hold_inspection(authority: &Authority) -> (Receiver<()>, SyncSender<()>) {
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
    (entered, release_tx)
}

fn source_picture(fx: &Attached) -> Vec<u8> {
    std::fs::read(
        folder(fx.root.path(), fx.bot.main_chat, fx.picture.attachment.id)
            .join("file")
            .join("fixture.png"),
    )
    .unwrap()
}

#[test]
fn bot_branch_with_attached_files_copies_them_and_commits_with_its_association() {
    let fx = attached();
    let source_messages = fx.repo.messages(fx.bot.main_chat).unwrap();
    let source_records = fx.repo.attachment_records(fx.bot.main_chat).unwrap();
    let Attached {
        root,
        repo,
        authority,
        bot,
        through,
        picture,
        reference,
    } = fx;
    let (worker, client, returned) = worker_on(root.path().to_owned(), repo, authority);
    let destination = ChatId::new();
    let asked = request(destination, through, "Branch");
    let branched = client
        .branch_chat(context(true), bot.main_chat, asked.clone())
        .unwrap()
        .transcript;
    assert_eq!(branched.chat.id, destination);
    assert_eq!(branched.attachments.len(), 2);
    let user = branched.messages[0].id;
    for copy in &branched.attachments {
        assert_ne!(copy.id, picture.attachment.id);
        assert_ne!(copy.id, reference.attachment.id);
        assert_eq!(copy.message_id, Some(user));
    }
    let picture_copy = branched
        .attachments
        .iter()
        .find(|copy| copy.source != AttachmentSource::Path)
        .unwrap();
    let reference_copy = branched
        .attachments
        .iter()
        .find(|copy| copy.source == AttachmentSource::Path)
        .unwrap();
    // A path reference is copied as its row, as ordinary Branch copies it (O30).
    assert_eq!(
        reference_copy.reference_path,
        reference.attachment.reference_path
    );
    assert_eq!(
        std::fs::read(
            folder(root.path(), destination, picture_copy.id)
                .join("file")
                .join("fixture.png")
        )
        .unwrap(),
        b"source picture"
    );
    assert_eq!(
        chat_folder_entries(root.path(), destination),
        vec![picture_copy.id.to_storage()]
    );
    // A lost reply retries to the same branch; any other reuse conflicts.
    let again = client
        .branch_chat(context(true), bot.main_chat, asked)
        .unwrap()
        .transcript;
    assert_eq!(again.chat, branched.chat);
    assert_eq!(again.attachments, branched.attachments);
    assert_eq!(
        client
            .branch_chat(
                context(true),
                bot.main_chat,
                request(destination, through, "Renamed")
            )
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(
        chat_folder_entries(root.path(), destination),
        vec![picture_copy.id.to_storage()]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), Some(bot.id));
    // O22: messages and attachments only; nothing is launched or copied besides.
    assert!(repo.attempts_for_chat(destination).unwrap().is_empty());
    assert!(repo.tool_activities(destination).unwrap().is_empty());
    // The source is untouched, rows and files.
    assert_eq!(repo.messages(bot.main_chat).unwrap(), source_messages);
    assert_eq!(
        repo.attachment_records(bot.main_chat).unwrap(),
        source_records
    );
    assert_eq!(
        std::fs::read(
            folder(root.path(), bot.main_chat, picture.attachment.id)
                .join("file")
                .join("fixture.png")
        )
        .unwrap(),
        b"source picture"
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot));
}

#[test]
fn a_duplicate_while_files_copy_is_busy_and_revoked_authority_refuses_before_commit() {
    let fx = attached();
    let destination = ChatId::new();
    let asked = request(destination, fx.through, "Branch");
    let records = fx.repo.attachment_records(fx.bot.main_chat).unwrap();
    let Attached {
        root,
        repo,
        authority,
        bot,
        picture,
        ..
    } = fx;
    let (worker, client, returned) = worker_on(root.path().to_owned(), repo, authority.clone());
    worker.ping().unwrap();
    let (entered, release) = hold_inspection(&authority);
    let first = send_branch(
        &client,
        prepared(&authority, &bot, destination),
        bot.main_chat,
        asked.clone(),
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The files are copied under the copy's own fresh identity; nothing is committed.
    let copied = chat_folder_entries(root.path(), destination);
    assert_eq!(copied.len(), 1);
    assert_ne!(copied[0], picture.attachment.id.to_storage());
    let duplicate = send_branch(
        &client,
        prepared(&authority, &bot, destination),
        bot.main_chat,
        asked.clone(),
    )
    .recv_timeout(Duration::from_secs(5))
    .unwrap()
    .unwrap_err();
    assert_eq!(duplicate.code, "chat_busy");
    assert!(duplicate.retryable);
    // Authority is revoked while the copy waits for its commit.
    authority.allowed.store(false, Ordering::SeqCst);
    release.send(()).unwrap();
    let refused = first
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, "wrong_owner");
    wait_until(|| chat_folder_entries(root.path(), destination).is_empty());
    assert_eq!(
        std::fs::read(
            folder(root.path(), bot.main_chat, picture.attachment.id)
                .join("file")
                .join("fixture.png")
        )
        .unwrap(),
        b"source picture"
    );
    // The reservation was released: restored authority branches once, afresh.
    authority.allowed.store(true, Ordering::SeqCst);
    let branched = client
        .branch_chat(context(true), bot.main_chat, asked)
        .unwrap()
        .transcript;
    let copy = branched
        .attachments
        .iter()
        .find(|copy| copy.source != AttachmentSource::Path)
        .unwrap();
    assert_ne!(copy.id.to_storage(), copied[0]);
    assert_eq!(
        chat_folder_entries(root.path(), destination),
        vec![copy.id.to_storage()]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), Some(bot.id));
    assert_eq!(repo.attachment_records(bot.main_chat).unwrap(), records);
}

#[test]
fn a_bot_edited_while_files_copy_refuses_before_commit_and_removes_the_copy() {
    let fx = attached();
    let destination = ChatId::new();
    let Attached {
        root,
        repo,
        authority,
        bot,
        through,
        picture,
        ..
    } = fx;
    let (worker, client, returned) = worker_on(root.path().to_owned(), repo, authority.clone());
    worker.ping().unwrap();
    let (entered, release) = hold_inspection(&authority);
    let first = send_branch(
        &client,
        prepared(&authority, &bot, destination),
        bot.main_chat,
        request(destination, through, "Branch"),
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // An edit lands on the writer while the copy waits; it carries its own
    // proof, so it does not queue behind the held inspection.
    let mut config = bot.config.clone();
    config.name = "Renamed while copying".into();
    let (reply, edited) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: prepared(&authority, &bot, bot.main_chat),
            request: BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                config,
            },
            reply,
        })
        .unwrap();
    assert!(edited.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    release.send(()).unwrap();
    let refused = first
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, "revision_conflict");
    wait_until(|| chat_folder_entries(root.path(), destination).is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), None);
    assert!(repo.messages(destination).unwrap().is_empty());
    assert!(repo.attachment_records(destination).unwrap().is_empty());
    assert!(
        folder(root.path(), bot.main_chat, picture.attachment.id)
            .join("file")
            .join("fixture.png")
            .is_file()
    );
}

#[test]
fn a_late_database_failure_rolls_back_and_removes_only_the_copys_files() {
    let fx = attached();
    let before = source_picture(&fx);
    let Attached {
        root,
        repo,
        authority,
        bot,
        through,
        picture,
        ..
    } = fx;
    repo.refuse_bot_branch_commit_for_fixture(true).unwrap();
    let records = repo.attachment_records(bot.main_chat).unwrap();
    let (worker, client, returned) = worker_on(root.path().to_owned(), repo, authority);
    let destination = ChatId::new();
    let failure = client
        .branch_chat(
            context(true),
            bot.main_chat,
            request(destination, through, "Branch"),
        )
        .unwrap_err();
    assert_eq!(failure.code, "storage_error");
    assert!(failure.retryable);
    wait_until(|| chat_folder_entries(root.path(), destination).is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), None);
    assert!(repo.chat(destination).unwrap().is_none());
    assert!(repo.messages(destination).unwrap().is_empty());
    assert!(repo.attachment_records(destination).unwrap().is_empty());
    assert_eq!(repo.attachment_records(bot.main_chat).unwrap(), records);
    assert_eq!(
        std::fs::read(
            folder(root.path(), bot.main_chat, picture.attachment.id)
                .join("file")
                .join("fixture.png")
        )
        .unwrap(),
        before
    );
}

/// The shared Branch copy of the fixture's prefix into a fresh destination,
/// built as `branch_copy` builds it.
fn bot_copy(repo: &Repository, bot: &Bot, through: MessageId) -> BranchCopy {
    let source = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let destination = ChatId::new();
    let mut ids = std::collections::HashMap::new();
    let messages = repo
        .messages(bot.main_chat)
        .unwrap()
        .into_iter()
        .map(|mut message| {
            let origin = message.id;
            message.id = MessageId::new();
            message.chat_id = destination;
            message.origin_message_id = Some(origin);
            ids.insert(origin, message.id);
            message
        })
        .collect();
    let attachments = repo
        .attachment_records(bot.main_chat)
        .unwrap()
        .into_iter()
        .filter_map(|mut record| {
            let message = *ids.get(&record.attachment.message_id?)?;
            record.chat_id = destination;
            record.attachment.id = ChatAttachmentId::new();
            record.attachment.message_id = Some(message);
            Some(record)
        })
        .collect();
    let mut tool_policy = repo.chat_tool_policy(bot.main_chat).unwrap().unwrap();
    tool_policy.chat_id = destination;
    BranchCopy {
        chat: Chat {
            id: destination,
            title: "Branch".into(),
            branched_from: Some(BranchOrigin {
                chat_id: bot.main_chat,
                through_message_id: through,
            }),
            ..source
        },
        tool_policy,
        messages,
        blocks: vec![],
        citations: vec![],
        pinned_documents: vec![],
        attachments,
    }
}

fn fresh_files(copy: &BranchCopy) -> Vec<(ChatId, ChatAttachmentId)> {
    copy.attachments
        .iter()
        .filter(|record| record.attachment.source != AttachmentSource::Path)
        .map(|record| (record.chat_id, record.attachment.id))
        .collect()
}

#[test]
fn an_ambiguous_commit_is_settled_by_what_is_stored_and_never_removes_a_committed_folder() {
    let Attached {
        mut repo,
        bot,
        through,
        picture,
        ..
    } = attached();
    let (cleanup, jobs) = Cleanup::recording();
    // Not committed: only the copy's own fresh picture identity goes; never a
    // path reference, never a chat folder, never the source's picture.
    let copy = bot_copy(&repo, &bot, through);
    assert!(!settle_uncommitted(&repo, &cleanup, &bot, &copy));
    let CleanupJob::RemoveAttachments(removed) = jobs.try_recv().unwrap() else {
        panic!("a failed branch removes only its own attachment identities");
    };
    assert_eq!(removed, fresh_files(&copy));
    assert!(
        removed
            .iter()
            .all(|(chat, id)| *chat == copy.chat.id && *id != picture.attachment.id)
    );
    // Committed although the caller saw an error: every folder stays.
    repo.create_bot_branch(&bot, &copy).unwrap();
    assert!(settle_uncommitted(&repo, &cleanup, &bot, &copy));
    assert!(jobs.try_recv().is_err());
    // A destination someone else created meanwhile with the same origin and
    // title is not this copy's: its own rows and folders are left alone.
    let other = bot_copy(&repo, &bot, through);
    repo.create_chat(&other.chat).unwrap();
    let existing = branch_attachment(other.chat.id);
    repo.stage_attachment(&existing).unwrap();
    assert!(!settle_uncommitted(&repo, &cleanup, &bot, &other));
    let CleanupJob::RemoveAttachments(removed) = jobs.try_recv().unwrap() else {
        panic!("a failed branch removes only its own attachment identities");
    };
    assert_eq!(removed, fresh_files(&other));
    assert!(!removed.iter().any(|(_, id)| *id == existing.attachment.id));
    assert_eq!(
        repo.attachment(existing.attachment.id).unwrap(),
        Some(existing)
    );
}
