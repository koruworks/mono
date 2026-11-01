// SPDX-License-Identifier: MPL-2.0

//! A reader's private document state through the real Store and SQLite
//! repository, in memory: absent reads write nothing, writes merge and never
//! touch the shared document, permanent deletion — which reaches the Store
//! only as a manifest commit — removes the rows, trash keeps them, and a move
//! between workspaces carries them.

use super::*;
use koru_store_core::document_private::{COLLAPSED, PrivateChange};
use koru_store_core::errors::{Code, Result};
use koru_store_core::manifest::Manifest;
use koru_store_core::ports::UpdateOrigin;
use koru_store_core::service::Backend;
use koru_store_core::{DocumentId, ProfileId};

struct MemoryBackend;
impl Backend for MemoryBackend {
    type Workspace = crate::SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
        crate::SqliteWorkspace::in_memory()
    }
    fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
        panic!("private state never deletes a Workspace")
    }
    fn objects(&self) -> Result<Self::Blobs> {
        panic!("private state never opens blobs")
    }
}

const NODE_A: &str = "0190f1d2-0000-7000-8000-00000000000a";
const NODE_B: &str = "0190f1d2-0000-7000-8000-00000000000b";

fn fold(node: &str, closed: bool) -> PrivateChange {
    PrivateChange {
        node_id: node.to_owned(),
        key: COLLAPSED.to_owned(),
        value: Some(closed),
    }
}

struct Fixture {
    store: Store<SqliteCatalog, MemoryBackend>,
    profile: ProfileId,
    workspace: WorkspaceId,
    document: DocumentId,
}

fn fixture() -> Fixture {
    let mut store = Store::open(SqliteCatalog::in_memory().unwrap(), MemoryBackend).unwrap();
    let profile = store
        .create_profile("Reader", None, None, Locale::default(), at())
        .unwrap()
        .id;
    let workspace = store.create_workspace(profile, "ws", at()).unwrap().id;
    let (document, _) = store
        .create_document(workspace, "Notes", "document", None, None, at())
        .unwrap();
    Fixture {
        store,
        profile,
        workspace,
        document,
    }
}

impl Fixture {
    fn state(&mut self, owner: ProfileId) -> koru_store_core::document_private::PrivateState {
        self.store
            .document_private_state(self.workspace, self.document, owner)
            .unwrap()
    }

    fn apply(&mut self, changes: &[PrivateChange]) -> Result<()> {
        self.store
            .apply_document_private_changes(
                self.workspace,
                self.document,
                self.profile,
                changes,
                at(),
            )
            .map(drop)
    }

    /// What another device's permanent delete looks like on arrival: a
    /// manifest update, applied and flushed like any synced change.
    fn remove_through_the_manifest(&mut self) {
        let manifest_id = self.store.manifest_id(self.workspace).unwrap();
        let bytes = self.store.snapshot(self.workspace, manifest_id).unwrap();
        let peer = Manifest::from_snapshot(&bytes).unwrap();
        peer.set_peer_id(0xfeed).unwrap();
        let mut peer = peer;
        let before = peer.version();
        peer.remove_document(self.document).unwrap();
        let delta = peer.export_updates_since(&before).unwrap();
        self.store
            .apply_update(
                self.workspace,
                manifest_id,
                delta,
                at(),
                UpdateOrigin::Store,
            )
            .unwrap();
        self.store.flush(self.workspace, manifest_id, at()).unwrap();
    }
}

#[test]
fn an_absent_read_is_open_and_a_fold_is_the_readers_own() {
    let mut f = fixture();
    let shared = |f: &mut Fixture| {
        (
            f.store.snapshot(f.workspace, f.document).unwrap(),
            f.store
                .document_updated_at(f.workspace, f.document)
                .unwrap(),
        )
    };
    let before = shared(&mut f);
    assert!(f.state(f.profile).nodes.is_empty());

    f.apply(&[fold(NODE_A, true)]).unwrap();
    let state = f.state(f.profile);
    assert!(state.nodes[NODE_A][COLLAPSED]);
    assert!(state.revision > 0);

    // Folding is not a shared edit: same bytes, same edit time.
    assert_eq!(shared(&mut f), before);

    // Another local Profile sees its own, empty, state.
    let other = f
        .store
        .create_profile("Other", None, None, Locale::default(), at())
        .unwrap()
        .id;
    assert!(f.state(other).nodes.is_empty());
}

#[test]
fn an_invalid_batch_or_unknown_target_writes_nothing() {
    let mut f = fixture();
    let bad = PrivateChange {
        node_id: NODE_B.to_owned(),
        key: "koru.unknown".to_owned(),
        value: Some(true),
    };
    assert!(f.apply(&[fold(NODE_A, true), bad]).is_err());
    assert!(f.state(f.profile).nodes.is_empty());

    let stranger = ProfileId::new();
    let error = f
        .store
        .apply_document_private_changes(
            f.workspace,
            f.document,
            stranger,
            &[fold(NODE_A, true)],
            at(),
        )
        .unwrap_err();
    assert_eq!(error.code(), Code::ProfileNotFound);

    let missing = f
        .store
        .apply_document_private_changes(
            f.workspace,
            DocumentId::new(),
            f.profile,
            &[fold(NODE_A, true)],
            at(),
        )
        .unwrap_err();
    assert_eq!(missing.code(), Code::DocumentNotFound);
}

#[test]
fn trash_keeps_folds_and_a_permanent_delete_removes_them() {
    let mut f = fixture();
    f.apply(&[fold(NODE_A, true)]).unwrap();
    f.store
        .trash_document_entry(f.workspace, f.document, at(), None)
        .unwrap();
    assert!(f.state(f.profile).nodes[NODE_A][COLLAPSED]);
    assert_eq!(
        f.store
            .document_private_owners(f.workspace, f.document)
            .unwrap(),
        vec![f.profile]
    );

    f.remove_through_the_manifest();
    // The shared bytes stay recoverable; the private rows do not.
    assert!(f.store.snapshot(f.workspace, f.document).is_ok());
    assert!(
        f.store
            .document_private_owners(f.workspace, f.document)
            .unwrap()
            .is_empty()
    );
    // A queued write for the deleted document is refused rather than
    // resurrecting private state.
    assert_eq!(
        f.apply(&[fold(NODE_B, true)]).unwrap_err().code(),
        Code::DocumentNotFound
    );
}

#[test]
fn a_move_between_workspaces_carries_the_readers_folds() {
    let mut f = fixture();
    f.apply(&[fold(NODE_A, true)]).unwrap();
    let target = f
        .store
        .create_workspace(f.profile, "dest", at())
        .unwrap()
        .id;
    f.store
        .transfer_documents(f.workspace, target, f.document, None, 0, at())
        .unwrap();
    let state = f
        .store
        .document_private_state(target, f.document, f.profile)
        .unwrap();
    assert!(state.nodes[NODE_A][COLLAPSED]);
    assert!(
        f.store
            .document_private_owners(f.workspace, f.document)
            .unwrap()
            .is_empty(),
        "the source rows went with the source bytes"
    );
}
