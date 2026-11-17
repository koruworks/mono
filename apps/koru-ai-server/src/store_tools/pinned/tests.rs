// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_save_keeps_version_id` (the Store half), `m6_pinned_read`,
//! `m6_pinned_unavailable` and `m6_pinned_authority`, over a real Store kept
//! in memory. No disk, no delegate, no provider.
use super::*;
use crate::store_tools::native_store_backend;
use koru_core::{Node, Parent, WorkspaceDoc};
use koru_mcp_server::Backend;
use koru_mcp_server::native::{NativeStoreIdentity, NativeWriteEvidence, NativeWriteKind};
use koru_store_core::ids::ProfileId;
use koru_store_core::ports::MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES;
use koru_store_server::memory::MemoryStore;
use serde_json::json;

/// What the child wrote, what a person wrote after it, and the page's title.
const PINNED: &str = "PINNED-SENTINEL the child's sentence";
const LIVE: &str = "LIVE-SENTINEL a person's later sentence";
const SECOND: &str = "SECOND-SENTINEL the child's second save";

struct Fixture {
    memory: MemoryStore,
    authority: NativeStoreAuthority,
    other_profile: ProfileId,
}

fn fixture() -> Fixture {
    let memory = MemoryStore::open("pinned-read-memory").unwrap();
    let (authority, other_profile) = memory
        .with_store(|store| {
            let at = Timestamp::now();
            let profile = store
                .create_profile("First", None, None, koru_store_core::Locale::default(), at)
                .unwrap()
                .id;
            let other = store
                .create_profile("Second", None, None, koru_store_core::Locale::default(), at)
                .unwrap()
                .id;
            let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
            (
                NativeStoreAuthority {
                    owner_scope: store.local_user_scope().unwrap(),
                    expected_profile: profile,
                    workspace,
                },
                other,
            )
        })
        .unwrap();
    Fixture {
        memory,
        authority,
        other_profile,
    }
}

fn identity() -> NativeStoreIdentity {
    NativeStoreIdentity {
        candidate: DocumentId::new(),
        scope: "pinned-read-test".into(),
        request_key: uuid::Uuid::new_v4().to_string(),
    }
}

impl Fixture {
    /// The child's save of a new page, through the native effect path.
    fn create(&self, title: &str) -> NativeWriteEvidence {
        let backend = native_store_backend(self.memory.client(), self.authority.clone());
        let prepared = backend
            .prepare_native_write(
                "create_document",
                json!({
                    "workspace_id": self.authority.workspace,
                    "title": title,
                    "initial_operations": [{
                        "op": "insert", "node_id": "body", "parent_id": "root",
                        "after_node_id": null, "kind": "paragraph", "text": PINNED
                    }]
                }),
            )
            .unwrap();
        backend
            .execute_native_write(&prepared, identity())
            .unwrap_or_else(|error| panic!("create refused: {}", error.error().code))
    }

    /// The child's later save of the same page, through the native effect path.
    fn edit(&self, document: DocumentId, text: &str) -> NativeWriteEvidence {
        let backend = native_store_backend(self.memory.client(), self.authority.clone());
        let read = backend
            .invoke(
                &RequestContext::local(),
                "read_document",
                json!({"workspace_id": self.authority.workspace, "document_id": document}),
            )
            .unwrap();
        let prepared = backend
            .prepare_native_write(
                "edit_document",
                json!({
                    "workspace_id": self.authority.workspace,
                    "document_id": document,
                    "base_revision": read["revision"],
                    "operations": [{
                        "op": "insert", "parent_id": read["root_node_id"],
                        "after_node_id": null, "kind": "paragraph", "text": text
                    }]
                }),
            )
            .unwrap();
        let mut key = identity();
        key.candidate = document;
        backend
            .execute_native_write(&prepared, key)
            .unwrap_or_else(|error| panic!("edit refused: {}", error.error().code))
    }

    /// A person's own edit, made in the app after the child saved.
    fn person_edits(&self, document: DocumentId) {
        let workspace = self.authority.workspace;
        self.memory
            .with_store(move |store| {
                let mut doc =
                    WorkspaceDoc::from_snapshot(&store.snapshot(workspace, document).unwrap())
                        .unwrap();
                let before = doc.version();
                let root = doc.children(Parent::Root).unwrap()[0];
                doc.insert(Parent::Node(root), Node::paragraph(LIVE).unwrap())
                    .unwrap();
                store
                    .apply_update(
                        workspace,
                        document,
                        doc.export_updates_since(&before).unwrap(),
                        Timestamp::now(),
                        None,
                    )
                    .unwrap();
            })
            .unwrap();
    }

    fn live_text(&self, document: DocumentId) -> String {
        let workspace = self.authority.workspace;
        let bytes = self
            .memory
            .with_store(move |store| store.snapshot(workspace, document).unwrap())
            .unwrap();
        koru_mcp_server::native::render_pinned_document(document, &bytes)
            .unwrap()
            .to_string()
    }

    fn read(
        &self,
        document: DocumentId,
        version: Option<DocumentVersionId>,
    ) -> Result<PinnedPage, PinnedReadError> {
        read_pinned_page(
            self.memory.client(),
            self.authority.clone(),
            document,
            version,
        )
    }
}

fn text(page: &PinnedPage) -> String {
    page.content.to_string()
}

// ---- m6_save_keeps_version_id ------------------------------------------------

#[test]
fn m6_save_keeps_version_id_create_and_edit_evidence_carry_the_stores_id() {
    let f = fixture();
    let created = f.create("Result");
    assert_eq!(created.kind, NativeWriteKind::Created);
    let first = created.version_id.expect("a create returns the Store's id");
    let edited = f.edit(created.document, SECOND);
    assert_eq!(edited.kind, NativeWriteKind::Edited);
    assert_eq!(edited.document, created.document);
    let second = edited.version_id.expect("an edit returns the Store's id");
    assert_ne!(
        first, second,
        "each save produced its own immutable version"
    );
    // Each id names, in the Store, exactly the state its evidence describes.
    let workspace = f.authority.workspace;
    for evidence in [&created, &edited] {
        let id = evidence.version_id.unwrap();
        let stored = f
            .memory
            .with_store(move |store| store.bounded_document_version(workspace, id).unwrap())
            .unwrap()
            .expect("the version row exists");
        assert_eq!(stored.id, id);
        assert_eq!(stored.document, evidence.document);
        assert_eq!(stored.version_vector, evidence.version);
        assert_eq!(
            koru_store_core::lifecycle::checksum(&stored.snapshot),
            stored.checksum
        );
    }
}

// ---- m6_pinned_read ----------------------------------------------------------

#[test]
fn m6_pinned_read_returns_the_pinned_bytes_after_a_person_edits_the_page() {
    let f = fixture();
    let created = f.create("Result");
    let pinned = created.version_id.unwrap();
    f.person_edits(created.document);
    // The live page has the person's sentence. The pinned read does not.
    assert!(f.live_text(created.document).contains(LIVE));
    let page = f.read(created.document, Some(pinned)).unwrap();
    assert_eq!(page.document, created.document);
    assert_eq!(page.version_id, pinned);
    assert!(text(&page).contains(PINNED));
    assert!(!text(&page).contains(LIVE));
    // It is rendered as `read_document` renders a page.
    assert_eq!(page.content["document_id"], json!(created.document));
    assert!(page.content["root_node_id"].is_string());
    // The child edits again in the same Run: the handed-over version is the
    // last save's, and each id still reads its own state.
    let edited = f.edit(created.document, SECOND);
    let last = edited.version_id.unwrap();
    let latest = f.read(created.document, Some(last)).unwrap();
    assert!(text(&latest).contains(SECOND) && text(&latest).contains(LIVE));
    let earlier = f.read(created.document, Some(pinned)).unwrap();
    assert_eq!(earlier, page);
    assert!(!text(&earlier).contains(SECOND));
    // Reading changes nothing: the same answer twice.
    assert_eq!(f.read(created.document, Some(last)).unwrap(), latest);
    // No Debug output of the page carries its content.
    assert!(!format!("{page:?}").contains("SENTINEL"));
}

#[test]
fn m6_pinned_read_verifies_the_checksum_and_the_versions_own_page() {
    let f = fixture();
    let created = f.create("Result");
    let other = f.create("Another page");
    let workspace = f.authority.workspace;
    let id = created.version_id.unwrap();
    let stored = f
        .memory
        .with_store(move |store| store.bounded_document_version(workspace, id).unwrap())
        .unwrap()
        .unwrap();
    // As stored, it verifies.
    assert!(verified(stored.clone(), created.document).is_ok());
    // A checksum that is not the bytes'.
    let mut wrong_checksum = stored.clone();
    wrong_checksum.checksum = koru_store_core::lifecycle::checksum(b"other bytes");
    assert_eq!(
        verified(wrong_checksum, created.document),
        Err(PinnedUnavailable::Corrupt)
    );
    // Bytes that are not the checksum's: one flipped byte.
    let mut wrong_bytes = stored.clone();
    let last = wrong_bytes.snapshot.len() - 1;
    wrong_bytes.snapshot[last] ^= 0xff;
    assert_eq!(
        verified(wrong_bytes, created.document),
        Err(PinnedUnavailable::Corrupt)
    );
    // A version of another page is refused, by the value and by the read.
    assert_eq!(
        verified(stored, other.document),
        Err(PinnedUnavailable::Missing)
    );
    assert_eq!(
        f.read(other.document, Some(id)),
        Err(PinnedReadError::Unavailable(PinnedUnavailable::Missing))
    );
    assert_eq!(
        f.read(created.document, other.version_id),
        Err(PinnedReadError::Unavailable(PinnedUnavailable::Missing))
    );
}

// ---- m6_pinned_unavailable ---------------------------------------------------

#[test]
fn m6_pinned_unavailable_each_case_answers_its_reason_and_never_the_live_page() {
    let f = fixture();
    let created = f.create("Result");
    let document = created.document;
    let workspace = f.authority.workspace;
    f.person_edits(document);
    assert!(f.live_text(document).contains(LIVE));
    let refused = |version: Option<DocumentVersionId>| {
        let error = f.read(document, version).unwrap_err();
        // An error holds a code and a reason, and nothing of the page.
        let shown = format!("{error:?} {} {:?}", error.code(), error.reason());
        assert!(!shown.contains("SENTINEL"), "{shown}");
        error
    };
    // A missing id.
    let missing = refused(Some(DocumentVersionId::new()));
    assert_eq!(
        missing,
        PinnedReadError::Unavailable(PinnedUnavailable::Missing)
    );
    assert_eq!(
        (
            missing.code(),
            missing.reason().map(PinnedUnavailable::token),
            missing.retryable()
        ),
        ("result_version_unavailable", Some("missing"), false)
    );
    // An entry saved without a version id.
    let not_pinned = refused(None);
    assert_eq!(
        not_pinned.reason().map(PinnedUnavailable::token),
        Some("not_pinned")
    );
    // Bytes that carry their own checksum and are not a page.
    let (vv, frontiers) = (created.version.clone(), vec![1u8]);
    let record = |bytes: Vec<u8>, at: i64| {
        let (vv, frontiers) = (vv.clone(), frontiers.clone());
        f.memory
            .with_store(move |store| {
                store
                    .record_history_state(
                        workspace,
                        document,
                        &bytes,
                        &vv,
                        &[frontiers.as_slice(), &at.to_le_bytes()].concat(),
                        HistoryReason::Manual,
                        Timestamp::from_millis(at),
                    )
                    .unwrap()
                    .version
            })
            .unwrap()
    };
    let malformed = record(b"not a page at all".to_vec(), 8);
    let corrupt = refused(Some(malformed));
    assert_eq!(
        corrupt.reason().map(PinnedUnavailable::token),
        Some("corrupt")
    );
    // A snapshot one byte above the read bound.
    let oversized = record(vec![0u8; MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES + 1], 9);
    let too_large = refused(Some(oversized));
    assert_eq!(
        too_large.reason().map(PinnedUnavailable::token),
        Some("too_large")
    );
    assert_eq!(too_large.code(), "result_version_unavailable");
    // The page itself is still readable at its real pinned version.
    assert!(f.read(document, created.version_id).is_ok());
    // A dropped document: the page left this workspace and took its versions
    // with it. That is `missing`, and nothing of the page is read.
    let profile = f.authority.expected_profile;
    let pinned = created.version_id.unwrap();
    f.memory
        .with_store(move |store| {
            let at = Timestamp::now();
            let target = store.create_workspace(profile, "Elsewhere", at).unwrap().id;
            store
                .transfer_documents(workspace, target, document, None, 0, at)
                .unwrap();
            assert!(
                store
                    .bounded_document_version(workspace, pinned)
                    .unwrap()
                    .is_none(),
                "the version row went with the document"
            );
        })
        .unwrap();
    let dropped = refused(Some(pinned));
    assert_eq!(
        dropped,
        PinnedReadError::Unavailable(PinnedUnavailable::Missing)
    );
    // The Store not answering is an outage, never a missing version.
    let client = f.memory.client();
    let authority = f.authority.clone();
    f.memory.close();
    let outage = read_pinned_page(client, authority, document, Some(pinned)).unwrap_err();
    assert_eq!(outage, PinnedReadError::StoreUnavailable);
    assert_eq!(
        (outage.code(), outage.reason(), outage.retryable()),
        ("native_owner_unavailable", None, true)
    );
}

// ---- m6_pinned_authority -----------------------------------------------------

#[test]
fn m6_pinned_authority_a_page_in_trash_is_not_readable() {
    let f = fixture();
    let created = f.create("Result");
    let (document, pinned) = (created.document, created.version_id);
    let workspace = f.authority.workspace;
    assert!(f.read(document, pinned).is_ok());
    f.memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, document, Timestamp::now(), None)
                .unwrap();
        })
        .unwrap();
    let refused = f.read(document, pinned).unwrap_err();
    assert_eq!(refused, PinnedReadError::NotReadable);
    assert_eq!(
        (refused.code(), refused.reason(), refused.retryable()),
        ("result_not_readable", None, false)
    );
    // The version is still there: the refusal is the read check's, and an
    // entry without a version id says the same thing while the page is hidden.
    assert!(
        f.memory
            .with_store(move |store| store
                .bounded_document_version(workspace, pinned.unwrap())
                .unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(f.read(document, None), Err(PinnedReadError::NotReadable));
    // Another page of the same workspace is unaffected.
    let other = f.create("Still here");
    assert!(f.read(other.document, other.version_id).is_ok());
}

#[test]
fn m6_pinned_authority_the_check_uses_the_authority_passed_in() {
    let f = fixture();
    let created = f.create("Result");
    let (document, pinned) = (created.document, created.version_id);
    let read = |authority: NativeStoreAuthority| {
        read_pinned_page(f.memory.client(), authority, document, pinned)
    };
    assert!(read(f.authority.clone()).is_ok());
    // Another workspace of the same person: the page is not in it.
    let profile = f.authority.expected_profile;
    let elsewhere = f
        .memory
        .with_store(move |store| {
            store
                .create_workspace(profile, "Elsewhere", Timestamp::now())
                .unwrap()
                .id
        })
        .unwrap();
    let mut wrong_workspace = f.authority.clone();
    wrong_workspace.workspace = elsewhere;
    assert_eq!(
        read(wrong_workspace),
        Err(PinnedReadError::Unavailable(PinnedUnavailable::Missing)),
        "the version is not in that workspace, and nothing of the page is read"
    );
    // Another person.
    let mut wrong_scope = f.authority.clone();
    wrong_scope.owner_scope = koru_store_core::ids::UserScopeId::new();
    assert_eq!(read(wrong_scope), Err(PinnedReadError::NotReadable));
    // A profile the workspace is not placed in.
    let mut wrong_profile = f.authority.clone();
    wrong_profile.expected_profile = f.other_profile;
    assert_eq!(read(wrong_profile), Err(PinnedReadError::NotReadable));
    // The check is made now: after the workspace moves to another profile the
    // authority that read a moment ago is refused, and the new one reads.
    let (workspace, other) = (f.authority.workspace, f.other_profile);
    f.memory
        .with_store(move |store| store.move_workspace(workspace, other).unwrap())
        .unwrap();
    assert_eq!(read(f.authority.clone()), Err(PinnedReadError::NotReadable));
    let mut moved = f.authority.clone();
    moved.expected_profile = f.other_profile;
    assert!(read(moved).is_ok());
}
