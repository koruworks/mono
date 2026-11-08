// SPDX-License-Identifier: MPL-2.0

//! A chat's profile through the worker (AI schema 13, koru-ai D120), on a
//! real `ai.db` and a real Store in a temp dir.

use koru_ai_core::ProfileId;
use koru_store_core::Locale;

use super::*;

/// A Store with two profiles — *Private* first in Koru's order, then
/// *Work* — a workspace in each, and a document in Private's.
struct Profiles {
    store: koru_store_server::WorkerHandle,
    private: ProfileId,
    work: ProfileId,
    home: WorkspaceId,
    office: WorkspaceId,
    letter: DocumentId,
}

impl Profiles {
    fn new(root: &std::path::Path) -> Self {
        let store = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
            root.join("store"),
            "internal-debug",
        ))
        .spawn()
        .unwrap();
        let (private, work, home, office, letter) = store
            .client()
            .with_store(|store| {
                let locale = || Locale::parse("en").unwrap();
                let private = store.create_profile(
                    "Private",
                    None,
                    None,
                    locale(),
                    Timestamp::from_millis(1),
                )?;
                let work = store.create_profile(
                    "Work",
                    None,
                    None,
                    locale(),
                    Timestamp::from_millis(2),
                )?;
                let at = Timestamp::from_millis(3);
                let home = store.create_workspace(private.id, "Home", at)?;
                let office = store.create_workspace(work.id, "Office", at)?;
                let (letter, _) =
                    store.create_document(home.id, "Letter", "page", None, None, at)?;
                Ok::<_, koru_store_core::Error>((private.id, work.id, home.id, office.id, letter))
            })
            .unwrap()
            .unwrap();
        Self {
            store,
            private,
            work,
            home,
            office,
            letter,
        }
    }

    /// An AI worker over `root`'s `ai.db`, reading this Store.
    fn ai_worker(&self, root: &std::path::Path) -> WorkerHandle {
        AiWorker::new(
            WorkerConfig::deterministic_mock(root.join("ai"), "internal-debug").with_store_context(
                Arc::new(crate::StoreContextAdapter::new(self.store.client())),
            ),
        )
        .spawn()
        .unwrap()
    }

    fn finish(self, worker: WorkerHandle) {
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        assert_eq!(self.store.join(), koru_store_server::WorkerExit::Clean);
    }
}

fn profile_of(client: &AiWorkerClient, chat: ChatId) -> Option<ProfileId> {
    client.read_chat(context(), chat).unwrap().chat.profile_id
}

/// **The migration** (AI schema 12 -> 13): four chats written before
/// profiles, in a schema-12 `ai.db`, read in two profiles' workspaces or in
/// none. The worker places each before it serves anything: the profile of
/// the workspace it last read, a read before a grant, and with nothing at
/// all the first profile. Every chat and grant is kept.
#[test]
fn a_schema_12_stores_chats_go_to_the_profile_of_what_they_last_read() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    std::fs::create_dir_all(root.path().join("ai")).unwrap();
    let (a, b, c, d) = (ChatId::new(), ChatId::new(), ChatId::new(), ChatId::new());
    let message = koru_ai_core::MessageId::new();
    koru_ai_sqlite::write_at_schema(
        &root.path().join("ai").join(koru_ai_sqlite::DATABASE_FILE),
        12,
        &format!(
            "INSERT INTO chats (id, title, current_provider, current_model,
                                created_at_ms, updated_at_ms)
             VALUES ('{a}', 'A', 'mock', 'deterministic-v1', 1, 1),
                    ('{b}', 'B', 'mock', 'deterministic-v1', 2, 2),
                    ('{c}', 'C', 'mock', 'deterministic-v1', 3, 3),
                    ('{d}', 'D', 'mock', 'deterministic-v1', 4, 4);
             INSERT INTO chat_workspace_grants (chat_id, workspace_id, granted_at_ms)
             VALUES ('{a}', '{home}', 1), ('{b}', '{office}', 9), ('{d}', '{office}', 1);
             INSERT INTO messages (id, chat_id, ordinal, role, state, created_at_ms)
             VALUES ('{reply}', '{a}', 0, 'assistant', 'pending', 1);
             INSERT INTO inference_attempts (
                id, operation_id, chat_id, assistant_message_id, surface, provider,
                requested_model, prompt_template_version, status, queued_at_ms
             ) VALUES ('{attempt}', '{operation}', '{a}', '{reply}', 'standalone', 'mock',
                       'deterministic-v1', 'v1', 'pending', 1);
             INSERT INTO tool_invocations (
                id, attempt_id, chat_id, tool_name, read_only, status, approval,
                proposed_at_ms, completed_at_ms, target_workspace_id
             ) VALUES ('{read}', '{attempt}', '{a}', 'read_document', 1, 'complete',
                       'not_required', 4, 5, '{office}');
             INSERT INTO messages (id, chat_id, ordinal, role, state, created_at_ms)
             VALUES ('{message}', '{b}', 0, 'user', 'complete', 6);
             INSERT INTO message_document_refs (
                message_id, document_id, relation, display_title, source_revision, used_at_ms
             ) VALUES ('{message}', '{letter}', 'context', 'Letter', X'00', 7);",
            a = a.to_storage(),
            b = b.to_storage(),
            c = c.to_storage(),
            d = d.to_storage(),
            home = profiles.home.to_storage(),
            office = profiles.office.to_storage(),
            letter = profiles.letter.to_storage(),
            message = message.to_storage(),
            attempt = Uuid::new_v4(),
            operation = Uuid::new_v4(),
            read = Uuid::new_v4(),
            reply = Uuid::new_v4(),
        ),
    )
    .unwrap();

    let worker = profiles.ai_worker(root.path());
    assert_eq!(
        worker.info().unwrap().schema_version,
        koru_ai_sqlite::SCHEMA_VERSION
    );
    let client = worker.client();
    assert_eq!(profile_of(&client, a), Some(profiles.work), "a tool read");
    assert_eq!(
        profile_of(&client, b),
        Some(profiles.private),
        "a document read"
    );
    assert_eq!(
        profile_of(&client, c),
        Some(profiles.private),
        "no evidence"
    );
    assert_eq!(profile_of(&client, d), Some(profiles.work), "a grant");
    assert_eq!(client.list_chats(context(), None).unwrap().chats.len(), 4);
    let listed = |profile| {
        let mut ids: Vec<ChatId> = client
            .list_chats(context(), Some(profile))
            .unwrap()
            .chats
            .into_iter()
            .map(|chat| chat.id)
            .collect();
        ids.sort();
        ids
    };
    let sorted = |mut ids: Vec<ChatId>| {
        ids.sort();
        ids
    };
    assert_eq!(listed(profiles.work), sorted(vec![a, d]));
    assert_eq!(listed(profiles.private), sorted(vec![b, c]));
    assert_eq!(
        client
            .read_chat_tool_policy(context(), b)
            .unwrap()
            .policy
            .workspace_ids,
        vec![profiles.office],
        "a grant to another profile's workspace is kept"
    );
    profiles.finish(worker);
}

/// **A new chat is its window's profile's; a branch is its source's**
/// whichever window asks for it; a client that names no profile gets the
/// first one, as a chat with nothing read does.
#[test]
fn a_new_chat_takes_its_windows_profile_and_a_branch_its_sources() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let worker = profiles.ai_worker(root.path());
    let client = worker.client();

    let create = |profile| {
        client
            .create_chat(
                context(),
                CreateChatRequest {
                    chat_id: ChatId::new(),
                    title: "Profiled".into(),
                    profile_id: profile,
                    model: model(),
                },
            )
            .unwrap()
            .chat
    };
    let at_work = create(Some(profiles.work));
    assert_eq!(at_work.profile_id, Some(profiles.work));
    assert_eq!(profile_of(&client, at_work.id), Some(profiles.work));
    let unnamed = create(None);
    assert_eq!(
        unnamed.profile_id,
        Some(profiles.private),
        "the first profile"
    );

    let stream = send(&client, at_work.id, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let branch = client
        .branch_chat(
            context(),
            at_work.id,
            BranchChatRequest {
                branch_chat_id: ChatId::new(),
                through_message_id: stream.accepted.assistant_message_id,
                title: "Branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(branch.chat.profile_id, Some(profiles.work));
    profiles.finish(worker);
}

fn send_with(
    client: &AiWorkerClient,
    chat_id: ChatId,
    explicit_refs: Vec<ContextRef>,
) -> AiResponseStream {
    client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "Use this".into(),
                explicit_refs,
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap()
}

/// **A chat reads only its own profile** (AI schema 13), enforced here
/// from the stored chat, whatever the window: a document of another
/// profile's workspace is refused as a missing one is, and so is a block
/// linked from another profile's chat; the list and the search are one
/// profile's.
#[test]
fn a_chat_reads_and_lists_only_its_own_profile() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let create = |profile| {
        let chat_id = ChatId::new();
        client
            .create_chat(
                context(),
                CreateChatRequest {
                    chat_id,
                    title: "Profiled".into(),
                    profile_id: Some(profile),
                    model: model(),
                },
            )
            .unwrap();
        chat_id
    };
    let at_work = create(profiles.work);
    let at_home = create(profiles.private);
    let also_at_work = create(profiles.work);

    // Private's letter, from a chat at Work.
    let refused = send_with(
        &client,
        at_work,
        vec![ContextRef::Document {
            document_id: profiles.letter,
        }],
    );
    assert_eq!(
        wait_for_terminal(&client, refused.accepted.attempt_id),
        AttemptStatus::Failed
    );
    let missing = send_with(
        &client,
        also_at_work,
        vec![ContextRef::Document {
            document_id: DocumentId::new(),
        }],
    );
    assert_eq!(
        wait_for_terminal(&client, missing.accepted.attempt_id),
        AttemptStatus::Failed
    );
    let failure = |stream: &AiResponseStream| {
        client
            .read_attempt(context(), stream.accepted.attempt_id)
            .unwrap()
            .attempt
            .error_class
    };
    assert_eq!(
        failure(&refused),
        failure(&missing),
        "the same as no document"
    );

    // A block of a chat at Work: context for another chat at Work, not for
    // one in Private.
    let first = send(&client, at_work, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let block = client.read_chat(context(), at_work).unwrap().blocks[0].id;
    let same = send_with(
        &client,
        also_at_work,
        vec![ContextRef::MessageBlock { block_id: block }],
    );
    assert_eq!(
        wait_for_terminal(&client, same.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let other = send_with(
        &client,
        at_home,
        vec![ContextRef::MessageBlock { block_id: block }],
    );
    assert_eq!(
        wait_for_terminal(&client, other.accepted.attempt_id),
        AttemptStatus::Failed
    );

    let home_reply = send(&client, at_home, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, home_reply.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let listed = client
        .list_chats(context(), Some(profiles.private))
        .unwrap()
        .chats;
    assert_eq!(
        listed.iter().map(|chat| chat.id).collect::<Vec<_>>(),
        vec![at_home]
    );
    let search = |profile| {
        client
            .search(
                context(),
                AiSearchRequest {
                    text: "deterministic".into(),
                    limit: 50,
                    cursor: None,
                    request_seq: 0,
                    profile,
                },
            )
            .unwrap()
            .page
            .hits
    };
    let everywhere = search(None);
    assert!(everywhere.iter().any(|hit| hit.chat_id == at_home));
    let at_work_hits = search(Some(profiles.work));
    assert!(!at_work_hits.is_empty());
    assert!(
        at_work_hits
            .iter()
            .all(|hit| hit.profile_id == Some(profiles.work))
    );
    profiles.finish(worker);
}

/// **A reply is kept as a document only in its chat's profile** (AI
/// schema 13): an export to another profile's workspace is refused before
/// anything is written.
#[test]
fn a_chats_reply_is_kept_only_in_its_own_profile() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Profiled".into(),
                profile_id: Some(profiles.work),
                model: model(),
            },
        )
        .unwrap();
    let stream = send(&client, chat_id, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let refused = client
        .export_chat(
            context(),
            chat_id,
            koru_ai_wire::ExportChatRequest {
                export_id: koru_ai_core::DocumentExportId::new(),
                idempotency_key: Uuid::new_v4(),
                through_message_id: stream.accepted.assistant_message_id,
                destination: DocumentDestination {
                    profile_id: profiles.private,
                    workspace_id: profiles.home,
                    parent_document_id: None,
                    index: None,
                    title: "Kept".into(),
                },
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, "forbidden");
    profiles.finish(worker);
}

// ----- a deleted profile's chats go with it ---------------------------------

impl Profiles {
    /// One more profile, with no workspace, so it can be removed at once.
    fn add_profile(&self, name: &'static str) -> ProfileId {
        self.store
            .client()
            .with_store(move |store| {
                store
                    .create_profile(
                        name,
                        None,
                        None,
                        Locale::parse("en").unwrap(),
                        Timestamp::from_millis(10),
                    )
                    .map(|profile| profile.id)
            })
            .unwrap()
            .unwrap()
    }

    /// Remove `profile` from this computer at `at`, as Manage Profiles does
    /// once its workspaces are in the Trash.
    fn remove(&self, profile: ProfileId, at: Timestamp) {
        self.store
            .client()
            .with_store(move |store| {
                let mark = store.begin_profile_removal(
                    profile,
                    koru_store_core::profile_removal::RemovalKind::Local,
                    at,
                )?;
                store.remove_profile_local(profile, Some(mark.attempt), at)
            })
            .unwrap()
            .unwrap();
    }

    fn restore(&self, profile: ProfileId) {
        self.store
            .client()
            .with_store(move |store| store.restore_profile_local(profile))
            .unwrap()
            .unwrap();
    }
}

/// Long enough ago that the Trash has let go of what the profile had.
fn final_removal() -> Timestamp {
    Timestamp::from_millis(5)
}

fn stop(worker: WorkerHandle) {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

fn chat_in(client: &AiWorkerClient, profile: ProfileId, title: &str) -> ChatId {
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: title.into(),
                profile_id: Some(profile),
                model: model(),
            },
        )
        .unwrap();
    chat_id
}

fn chat_with_reply(client: &AiWorkerClient, profile: ProfileId) -> ChatId {
    let chat = chat_in(client, profile, "Talked");
    let stream = send(client, chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    chat
}

/// A chat's attachment folder on disk, with a file in it, as an upload
/// leaves one.
fn attachment_folder(root: &std::path::Path, chat: ChatId) -> std::path::PathBuf {
    let folder = root
        .join("ai")
        .join(crate::attachments::ATTACHMENTS_DIR)
        .join(chat.to_storage());
    let file = folder.join(Uuid::new_v4().to_string()).join("file");
    std::fs::create_dir_all(&file).unwrap();
    std::fs::write(file.join("notes.txt"), b"kept with the chat").unwrap();
    folder
}

fn listed(client: &AiWorkerClient) -> Vec<ChatId> {
    let mut ids: Vec<ChatId> = client
        .list_chats(context(), None)
        .unwrap()
        .chats
        .into_iter()
        .map(|chat| chat.id)
        .collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<ChatId>) -> Vec<ChatId> {
    ids.sort();
    ids
}

/// **Deleting a profile takes its chats with it** once the deletion is
/// final — every row and the attachment folders — and another profile's
/// chats are untouched. A profile removed only lately can still be
/// restored, so its chats wait, hidden, and come back with it.
#[test]
fn a_deleted_profiles_chats_go_with_it_once_its_deletion_is_final() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let gone = profiles.add_profile("Gone");
    let lately = profiles.add_profile("Lately");

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let gone_talked = chat_with_reply(&client, gone);
    let gone_quiet = chat_in(&client, gone, "Quiet");
    let lately_chat = chat_in(&client, lately, "Lately");
    let work_chat = chat_with_reply(&client, profiles.work);
    stop(worker);
    let gone_folder = attachment_folder(root.path(), gone_talked);
    let work_folder = attachment_folder(root.path(), work_chat);

    profiles.remove(gone, final_removal());
    profiles.remove(lately, Timestamp::now());

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    assert_eq!(
        listed(&client),
        vec![work_chat],
        "the deleted profile's chats are gone, the lately removed one's hidden"
    );
    assert_eq!(
        client.read_chat(context(), lately_chat).unwrap_err().code,
        "not_found"
    );
    for chat in [gone_talked, gone_quiet] {
        assert_eq!(
            client.read_chat(context(), chat).unwrap_err().code,
            "not_found"
        );
    }
    let work = client.read_chat(context(), work_chat).unwrap();
    assert_eq!(work.chat.profile_id, Some(profiles.work));
    assert_eq!(work.messages.len(), 2, "the other profile's chat is whole");
    stop(worker);
    assert!(!gone_folder.exists(), "its attachments went with it");
    assert!(work_folder.exists(), "another chat's attachments stay");

    profiles.restore(lately);
    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    assert_eq!(
        client
            .list_chats(context(), Some(lately))
            .unwrap()
            .chats
            .into_iter()
            .map(|chat| chat.id)
            .collect::<Vec<_>>(),
        vec![lately_chat],
        "a restored profile has its chats back"
    );
    assert_eq!(
        client.read_chat(context(), lately_chat).unwrap().chat.id,
        lately_chat,
        "untouched while it was hidden"
    );
    profiles.finish(worker);
}

/// **A retirement a crash cut short finishes at the next start**: one
/// chat's rows went and its folder did not, another chat was not reached
/// at all. Nothing recorded the pass; the Store's answer is asked again.
#[test]
fn a_retirement_cut_short_finishes_at_the_next_start() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let gone = profiles.add_profile("Gone");

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let reached = chat_with_reply(&client, gone);
    let not_reached = chat_with_reply(&client, gone);
    let work_chat = chat_in(&client, profiles.work, "Work");
    stop(worker);
    let reached_folder = attachment_folder(root.path(), reached);
    let not_reached_folder = attachment_folder(root.path(), not_reached);
    profiles.remove(gone, final_removal());

    // The crash: `reached`'s rows are committed away, its folder is not.
    let database = root.path().join("ai").join(koru_ai_sqlite::DATABASE_FILE);
    let mut repository = Repository::open(&database).unwrap();
    assert!(repository.delete_chat(reached).unwrap());
    drop(repository);

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    assert_eq!(listed(&client), vec![work_chat]);
    stop(worker);
    assert!(
        !reached_folder.exists(),
        "the start-up sweep took the orphan"
    );
    assert!(!not_reached_folder.exists(), "the rest of the pass ran");

    // And again: nothing is left to do, and nothing else is touched.
    let worker = profiles.ai_worker(root.path());
    assert_eq!(listed(&worker.client()), vec![work_chat]);
    profiles.finish(worker);
}

/// **A chat is never retired on what the Store does not know.** A profile
/// with no row here — another root's, one not synced yet — is not one
/// known to be gone, so its chat stays, attachments and all.
#[test]
fn a_chat_of_a_profile_the_store_does_not_know_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let stranger = ProfileId::new();

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let strangers = chat_in(&client, stranger, "Elsewhere");
    let work_chat = chat_in(&client, profiles.work, "Work");
    stop(worker);
    let folder = attachment_folder(root.path(), strangers);

    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    assert_eq!(listed(&client), sorted(vec![strangers, work_chat]));
    assert_eq!(
        client
            .read_chat(context(), strangers)
            .unwrap()
            .chat
            .profile_id,
        Some(stranger)
    );
    profiles.finish(worker);
    assert!(folder.exists());
}

// ----- a removed profile's chats are hidden ---------------------------------

fn searched(client: &AiWorkerClient, profile: Option<ProfileId>) -> Vec<ChatId> {
    let mut ids: Vec<ChatId> = client
        .search(
            context(),
            AiSearchRequest {
                text: "deterministic".into(),
                limit: 50,
                cursor: None,
                request_seq: 0,
                profile,
            },
        )
        .unwrap()
        .page
        .hits
        .into_iter()
        .map(|hit| hit.chat_id)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// What an outside MCP client lists, and what it finds.
fn tool_listed(client: &AiWorkerClient) -> Vec<ChatId> {
    let mut ids: Vec<ChatId> = client
        .tool_list_chats(context(), ListChatsToolRequest::default())
        .unwrap()
        .chats
        .into_iter()
        .map(|chat| chat.chat_id)
        .collect();
    ids.sort();
    ids
}

fn tool_searched(client: &AiWorkerClient) -> Vec<ChatId> {
    let mut ids: Vec<ChatId> = client
        .tool_search_chats(
            context(),
            SearchChatsToolRequest {
                query: "deterministic".into(),
                cursor: None,
                page_size: None,
            },
        )
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| hit.chat_id)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn tool_read(client: &AiWorkerClient, chat_id: ChatId) -> Result<u32, Failure> {
    client
        .tool_read_chat(
            context(),
            ReadChatToolRequest {
                chat_id,
                cursor: None,
                page_size: None,
            },
        )
        .map(|read| read.total_messages)
}

/// **A removed profile's chats are hidden everywhere from the moment it is
/// removed** (the user's call, 2026-11-04: "yes, hide") — the unscoped
/// list the recents come from, a search across profiles, an outside MCP
/// client's list, search and read — and one opened by its id, or by its
/// reply's, is not found, as a deleted chat is. Another profile's chats are
/// untouched, Clear history leaves the hidden ones alone, and a restore
/// brings them all back at once, in the same worker, with no restart.
#[test]
fn a_removed_profiles_chats_are_hidden_everywhere_until_it_is_restored() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let gone = profiles.add_profile("Gone");
    let worker = profiles.ai_worker(root.path());
    let client = worker.client();
    let gone_chat = chat_in(&client, gone, "Gone");
    let gone_reply = send(&client, gone_chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, gone_reply.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let gone_quiet = chat_in(&client, gone, "Quiet");
    let work_chat = chat_with_reply(&client, profiles.work);
    let everything = sorted(vec![gone_chat, gone_quiet, work_chat]);
    assert_eq!(listed(&client), everything);
    assert_eq!(searched(&client, None), sorted(vec![gone_chat, work_chat]));
    assert_eq!(tool_listed(&client), everything);

    profiles.remove(gone, Timestamp::now());

    // Every list and search, unscoped or for the profile itself.
    assert_eq!(listed(&client), vec![work_chat]);
    assert!(
        client
            .list_chats(context(), Some(gone))
            .unwrap()
            .chats
            .is_empty()
    );
    assert_eq!(searched(&client, None), vec![work_chat]);
    assert!(searched(&client, Some(gone)).is_empty());
    assert_eq!(tool_listed(&client), vec![work_chat]);
    assert_eq!(tool_searched(&client), vec![work_chat]);

    // By id, the answer a chat that never was gets.
    let missing = client.read_chat(context(), ChatId::new()).unwrap_err();
    for chat in [gone_chat, gone_quiet] {
        let refused = client.read_chat(context(), chat).unwrap_err();
        assert_eq!(
            (refused.code.as_str(), refused.retryable),
            (missing.code.as_str(), missing.retryable)
        );
    }
    let tool_missing = tool_read(&client, ChatId::new()).unwrap_err();
    assert_eq!(tool_missing.code, "chat_not_found");
    assert_eq!(
        tool_read(&client, gone_chat).unwrap_err().code,
        tool_missing.code
    );
    for code in [
        client
            .read_attempt(context(), gone_reply.accepted.attempt_id)
            .unwrap_err()
            .code,
        client
            .rename_chat(
                context(),
                gone_chat,
                RenameChatRequest {
                    title: "Renamed".into(),
                },
            )
            .unwrap_err()
            .code,
        client
            .pin_chat(
                context(),
                gone_chat,
                koru_ai_wire::PinChatRequest { pinned: true },
            )
            .unwrap_err()
            .code,
        client
            .feedback(
                context(),
                gone_reply.accepted.assistant_message_id,
                FeedbackRequest {
                    event_id: koru_ai_core::FeedbackEventId::new(),
                    value: koru_ai_core::FeedbackValue::Good,
                    surface: AiSurface::Standalone,
                },
            )
            .unwrap_err()
            .code,
        client
            .send_turn(
                context(),
                SendTurnRequest {
                    operation_id: Uuid::new_v4(),
                    chat_id: gone_chat,
                    surface: AiSurface::Standalone,
                    markdown: "Still here?".into(),
                    explicit_refs: Vec::new(),
                    web_search: false,
                    reasoning_override: None,
                    system_prompt_override: None,
                    replace_pinned_documents: false,
                    attachment_ids: Vec::new(),
                    answers: None,
                    skills: Vec::new(),
                },
            )
            .err()
            .expect("a removed profile's chat sends nothing")
            .code,
        client.delete_chat(context(), gone_quiet).unwrap_err().code,
        client
            .create_chat(
                context(),
                CreateChatRequest {
                    chat_id: ChatId::new(),
                    title: "New".into(),
                    profile_id: Some(gone),
                    model: model(),
                },
            )
            .unwrap_err()
            .code,
    ] {
        assert_eq!(code, "not_found");
    }

    // Another profile's chat is whole, and a clear takes only what shows.
    let work = client.read_chat(context(), work_chat).unwrap();
    assert_eq!(work.chat.profile_id, Some(profiles.work));
    assert_eq!(work.messages.len(), 2);
    assert_eq!(client.clear_history(context()).unwrap().chats_deleted, 1);
    assert!(listed(&client).is_empty());

    // Restored, in the same worker: everything of it is back as it was.
    profiles.restore(gone);
    assert_eq!(listed(&client), sorted(vec![gone_chat, gone_quiet]));
    assert_eq!(searched(&client, None), vec![gone_chat]);
    assert_eq!(tool_listed(&client), sorted(vec![gone_chat, gone_quiet]));
    assert_eq!(tool_read(&client, gone_chat).unwrap(), 2);
    let back = client.read_chat(context(), gone_chat).unwrap();
    assert_eq!(back.chat.title, "Gone", "never renamed");
    assert_eq!(back.chat.pinned_at, None, "never pinned");
    assert_eq!(back.messages.len(), 2, "never sent to");
    assert!(back.feedback.is_empty(), "never rated");
    assert_eq!(
        client
            .read_attempt(context(), gone_reply.accepted.attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );
    profiles.finish(worker);
}

/// **A reply running in a chat when its profile is removed is stopped** at
/// the worker's next command, as the person's Stop would: nothing goes on
/// in a chat nobody can see. What it wrote is kept, and is back with the
/// chat on a restore.
#[test]
fn a_reply_running_when_its_profile_is_removed_is_stopped_and_kept() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let gone = profiles.add_profile("Gone");
    let (captured, requests) = sync_channel(8);
    let (release, released) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(HeldProvider {
            requests: captured,
            release: Arc::new(std::sync::Mutex::new(released)),
        }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(root.path().join("ai"), "internal-debug", providers).with_store_context(
            Arc::new(crate::StoreContextAdapter::new(profiles.store.client())),
        ),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = chat_in(&client, gone, "Running");
    let stream = send(&client, chat, Uuid::new_v4());
    requests.recv_timeout(Duration::from_secs(5)).unwrap();

    profiles.remove(gone, Timestamp::now());
    assert!(listed(&client).is_empty());
    assert_eq!(
        client
            .cancel_attempt(context(), stream.accepted.attempt_id)
            .unwrap_err()
            .code,
        "not_found",
        "its Stop is a missing chat's too"
    );
    release.send(()).unwrap();

    profiles.restore(gone);
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Cancelled,
        "stopped, not completed after the removal"
    );
    let back = client.read_chat(context(), chat).unwrap();
    assert_eq!(back.messages.len(), 2, "the question and the stopped reply");
    profiles.finish(worker);
}

/// A Store that can be told to stop answering which profiles are removed,
/// and answers everything else as the real one does.
struct Unanswering {
    store: crate::StoreContextAdapter,
    silent: Arc<AtomicBool>,
}

impl StoreContextPort for Unanswering {
    fn resolve_document(
        &self,
        document: DocumentId,
        reader: Option<ProfileId>,
    ) -> Result<DocumentContext, AiError> {
        self.store.resolve_document(document, reader)
    }
    fn create_document(
        &self,
        idempotency_key: Uuid,
        destination: &DocumentDestination,
        markdown: &str,
    ) -> Result<DocumentId, AiError> {
        self.store
            .create_document(idempotency_key, destination, markdown)
    }
    fn profile_catalog(&self) -> Result<koru_ai_core::ProfileCatalog, AiError> {
        self.store.profile_catalog()
    }
    fn document_workspace(&self, document: DocumentId) -> Result<Option<WorkspaceId>, AiError> {
        self.store.document_workspace(document)
    }
    fn retired_profiles(&self) -> Result<Vec<ProfileId>, AiError> {
        self.store.retired_profiles()
    }
    fn removed_profiles(&self) -> Result<Vec<ProfileId>, AiError> {
        if self.silent.load(Ordering::Acquire) {
            return Err(AiError::new(
                AiErrorClass::Storage,
                "Koru's profiles are temporarily unavailable.",
                true,
            ));
        }
        self.store.removed_profiles()
    }
}

/// **A Store that does not say which profiles are removed shows nothing it
/// could be hiding**: every list, search and read is refused as retryable
/// — not served from an older answer a removal may have outdated — and
/// Stop still works. Once it answers again, the removed profile's chats are
/// hidden and the rest served as before.
#[test]
fn a_store_that_does_not_answer_refuses_rather_than_shows_a_removed_profiles_chats() {
    let root = tempfile::tempdir().unwrap();
    let profiles = Profiles::new(root.path());
    let gone = profiles.add_profile("Gone");
    let silent = Arc::new(AtomicBool::new(false));
    let worker = AiWorker::new(
        WorkerConfig::deterministic_mock(root.path().join("ai"), "internal-debug")
            .with_store_context(Arc::new(Unanswering {
                store: crate::StoreContextAdapter::new(profiles.store.client()),
                silent: Arc::clone(&silent),
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let gone_chat = chat_with_reply(&client, gone);
    let work_chat = chat_in(&client, profiles.work, "Work");
    let work_reply = send(&client, work_chat, Uuid::new_v4()).accepted.attempt_id;
    assert_eq!(
        wait_for_terminal(&client, work_reply),
        AttemptStatus::Complete
    );
    assert_eq!(listed(&client), sorted(vec![gone_chat, work_chat]));

    profiles.remove(gone, Timestamp::now());
    silent.store(true, Ordering::Release);
    let refusals = [
        client.list_chats(context(), None).unwrap_err(),
        client
            .list_chats(context(), Some(profiles.work))
            .unwrap_err(),
        client
            .search(
                context(),
                AiSearchRequest {
                    text: "deterministic".into(),
                    limit: 50,
                    cursor: None,
                    request_seq: 0,
                    profile: None,
                },
            )
            .unwrap_err(),
        client
            .tool_list_chats(context(), ListChatsToolRequest::default())
            .unwrap_err(),
        client.read_chat(context(), gone_chat).unwrap_err(),
        client.read_chat(context(), work_chat).unwrap_err(),
        tool_read(&client, gone_chat).unwrap_err(),
    ];
    for refused in refusals {
        assert_eq!(refused.code, "ai_storage");
        assert!(refused.retryable, "try again");
        assert_eq!(
            refused.message,
            "Koru's profiles are temporarily unavailable."
        );
    }
    assert_eq!(
        client.cancel_attempt(context(), work_reply).unwrap().status,
        AttemptStatus::Complete,
        "Stop is answered on the last answer: it shows nothing"
    );

    silent.store(false, Ordering::Release);
    assert_eq!(listed(&client), vec![work_chat]);
    assert_eq!(
        client.read_chat(context(), gone_chat).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        client
            .read_chat(context(), work_chat)
            .unwrap()
            .messages
            .len(),
        2
    );
    profiles.finish(worker);
}
