// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::Timestamp;
use koru_store_server::memory::MemoryStore;
mod write;

struct Fixture {
    memory: MemoryStore,
    adapter: BotAuthorityAdapter,
    reader: ProfileId,
    other: ProfileId,
    identity: BotMemoryIdentity,
    binding: BotMemoryBinding,
}
impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("bot-memory-adapter-fixture").unwrap();
        let (reader, other, home) = memory
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let reader = store
                    .create_profile("Reader", None, None, Default::default(), at)
                    .unwrap()
                    .id;
                let other = store
                    .create_profile("Other", None, None, Default::default(), at)
                    .unwrap()
                    .id;
                let home = store.create_workspace(reader, "Home", at).unwrap().id;
                (reader, other, home)
            })
            .unwrap();
        let adapter =
            BotAuthorityAdapter::from_device(memory.client(), &uuid::Uuid::new_v4().to_string())
                .unwrap();
        let owner = adapter.owner().unwrap();
        let identity = BotMemoryIdentity {
            owner,
            bot: BotId::mint(),
            workspace: home,
            creation_request: BotRequestId::mint(),
            candidate: koru_ai_core::DocumentId::new(),
            store_request: BotRequestId::mint(),
            seed_format: 1,
            seed_title: "Memory".into(),
            seed_hash: seed::seed_hash(home, "Memory").unwrap(),
        };
        let binding = BotMemoryBinding {
            owner,
            bot: identity.bot,
            page: BotPage {
                workspace: home,
                document: identity.candidate,
            },
            creation_request: identity.creation_request,
            bound_at_ms: 1,
        };
        Self {
            memory,
            adapter,
            reader,
            other,
            identity,
            binding,
        }
    }
    fn prepare(&self) -> PreparedMemoryPage {
        self.adapter
            .prepare_memory_page(
                self.identity.owner,
                self.reader,
                &self.identity,
                &Cancellation::default(),
            )
            .unwrap()
    }
    fn add_fact(&self, text: &str) {
        let page = self.binding.page.clone();
        let text = text.to_owned();
        self.memory
            .with_store(move |store| {
                let snapshot = store.snapshot(page.workspace, page.document).unwrap();
                let mut doc = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
                let root = koru_core::schema::genesis_root_id(koru_core::DocumentId::from_uuid(
                    page.document.as_uuid(),
                ));
                add_fact(&mut doc, root, &text);
                store
                    .apply_update(
                        page.workspace,
                        page.document,
                        doc.export_all_updates().unwrap(),
                        Timestamp::from_millis(2),
                        None,
                    )
                    .unwrap();
                store
                    .flush(page.workspace, page.document, Timestamp::from_millis(2))
                    .unwrap();
            })
            .unwrap();
    }
    fn origin(&self) -> CreationOrigin {
        let page = self.binding.page.clone();
        self.memory
            .with_store(move |store| {
                let CreationOriginLookup::Known(origin) = store
                    .bounded_creation_origin(page.workspace, page.document)
                    .unwrap()
                else {
                    panic!()
                };
                origin
            })
            .unwrap()
    }
}

#[test]
fn memory_creation_replays_first_seed_and_preserves_later_human_edit() {
    let f = Fixture::new();
    let PreparedMemoryPage::Confirmed(first) = f.prepare() else {
        panic!()
    };
    first.validate().unwrap();
    let origin = f.origin();
    let regenerated = seed::seed_update(&f.identity).unwrap();
    assert_ne!(regenerated, origin.initial_update);
    let PreparedMemoryPage::Confirmed(replayed) = f.prepare() else {
        panic!()
    };
    replayed.validate().unwrap();
    assert_eq!(f.origin(), origin);
    f.add_fact("Human fact stays here");
    assert_eq!(
        f.prepare(),
        PreparedMemoryPage::Disagreement(BotMemoryRefusal::DocumentChanged)
    );
    assert_eq!(f.origin(), origin);
    let capture = f
        .adapter
        .capture_memory(
            f.identity.owner,
            f.reader,
            &f.binding,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(capture.nodes.len(), 1);
}

#[test]
fn memory_capture_replay_uses_exact_history_after_live_edit() {
    let f = Fixture::new();
    assert!(matches!(f.prepare(), PreparedMemoryPage::Confirmed(_)));
    f.add_fact("Original fact");
    let prepared = f
        .adapter
        .capture_memory(
            f.identity.owner,
            f.reader,
            &f.binding,
            &Cancellation::default(),
        )
        .unwrap();
    let capture = BotLibraryCapture {
        version: 1,
        owner: f.identity.owner,
        bot: f.identity.bot,
        submission: BotRequestId::mint(),
        config_revision: 1,
        knowledge: Vec::new(),
        memory: BotCapturedMemory::Captured {
            page: prepared.page.clone(),
            version: prepared.version,
            creation_request: prepared.creation_request,
            nodes: prepared.nodes,
            omitted_count: prepared.omitted_count,
            invalid_count: prepared.invalid_count,
        },
        index_digest: prepared.index_digest,
    };
    let first = f
        .adapter
        .replay_memory_index(
            f.identity.owner,
            f.reader,
            f.identity.workspace,
            &capture,
            Some(&f.binding),
            &Cancellation::default(),
        )
        .unwrap();
    f.add_fact("Later fact");
    assert_eq!(
        f.adapter
            .replay_memory_index(
                f.identity.owner,
                f.reader,
                f.identity.workspace,
                &capture,
                Some(&f.binding),
                &Cancellation::default()
            )
            .unwrap(),
        first
    );
    let read = |snapshot| {
        f.adapter
            .read_memory_page(
                f.identity.owner,
                f.reader,
                &f.binding,
                snapshot,
                MemoryReadPosition::List {
                    after: 0,
                    limit: 20,
                },
                &Cancellation::default(),
            )
            .unwrap()
    };
    let MemoryPageResult::List {
        page: BotMemoryListPage::Observed { counts, .. },
        ..
    } = read(MemorySnapshot::Exact(prepared.version))
    else {
        panic!()
    };
    assert_eq!(counts.valid, 1);
    let MemoryPageResult::List {
        page: BotMemoryListPage::Observed { counts, .. },
        ..
    } = read(MemorySnapshot::Current)
    else {
        panic!()
    };
    assert_eq!(counts.valid, 2);
}

#[test]
fn memory_authority_refuses_foreign_owner_profile_trash_and_cancellation() {
    let f = Fixture::new();
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        f.adapter
            .prepare_memory_page(f.identity.owner, f.reader, &f.identity, &cancel),
        Err(BotError::Unavailable.into())
    );
    let page = f.binding.page.clone();
    assert!(matches!(
        f.memory
            .with_store(move |store| store
                .bounded_creation_origin(page.workspace, page.document)
                .unwrap())
            .unwrap(),
        CreationOriginLookup::Missing
    ));
    assert!(matches!(f.prepare(), PreparedMemoryPage::Confirmed(_)));
    let owner = f.identity.owner;
    for wrong_owner in [
        BotOwner {
            user_scope: UserScopeId::new(),
            ..owner
        },
        BotOwner {
            device: DeviceId::new(),
            ..owner
        },
    ] {
        assert_eq!(
            f.adapter
                .capture_memory(wrong_owner, f.reader, &f.binding, &Cancellation::default()),
            Err(BotError::Forbidden.into())
        );
    }
    assert_eq!(
        f.adapter
            .capture_memory(owner, f.other, &f.binding, &Cancellation::default()),
        Err(BotError::Forbidden.into())
    );
    let captured = f
        .adapter
        .capture_memory(owner, f.reader, &f.binding, &Cancellation::default())
        .unwrap();
    let page = f.binding.page.clone();
    f.memory
        .with_store(move |store| {
            store
                .trash_document_entry(
                    page.workspace,
                    page.document,
                    Timestamp::from_millis(3),
                    None,
                )
                .unwrap()
        })
        .unwrap();
    assert_eq!(
        f.prepare(),
        PreparedMemoryPage::Disagreement(BotMemoryRefusal::DocumentTrashed)
    );
    assert_eq!(
        f.adapter.read_memory_page(
            owner,
            f.reader,
            &f.binding,
            MemorySnapshot::Exact(captured.version),
            MemoryReadPosition::List {
                after: 0,
                limit: 20
            },
            &Cancellation::default()
        ),
        Err(BotError::Forbidden.into())
    );
}

#[test]
fn recorded_absent_replay_checks_empty_digest_and_never_reads_later_memory() {
    let f = Fixture::new();
    let mut capture = BotLibraryCapture {
        version: 1,
        owner: f.identity.owner,
        bot: f.identity.bot,
        submission: BotRequestId::mint(),
        config_revision: 1,
        knowledge: Vec::new(),
        memory: BotCapturedMemory::Absent {},
        index_digest: *blake3::hash(&[]).as_bytes(),
    };
    assert!(matches!(f.prepare(), PreparedMemoryPage::Confirmed(_)));
    f.add_fact("Later memory");
    assert!(
        f.adapter
            .replay_memory_index(
                f.identity.owner,
                f.reader,
                f.identity.workspace,
                &capture,
                Some(&f.binding),
                &Cancellation::default()
            )
            .unwrap()
            .bytes
            .is_empty()
    );
    capture.index_digest = [0; 32];
    assert_eq!(
        f.adapter.replay_memory_index(
            f.identity.owner,
            f.reader,
            f.identity.workspace,
            &capture,
            Some(&f.binding),
            &Cancellation::default()
        ),
        Err(MemoryAuthorityError::CaptureUnavailable)
    );
}
