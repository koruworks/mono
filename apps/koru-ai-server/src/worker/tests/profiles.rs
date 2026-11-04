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
    assert_eq!(client.list_chats(context()).unwrap().chats.len(), 4);
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
