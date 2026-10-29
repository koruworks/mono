// SPDX-License-Identifier: MPL-2.0

//! A Koru document, through a real Koru Store and back.
//!
//! **The first time `koru-workspace` has talked to the store at all.** Until
//! now the app's persistence was a folder and the store held nothing the editor
//! had written; this drives the actual seam — a `WorkspaceDoc` out through
//! `StoreSession`, into SQLite, and back into a second `WorkspaceDoc` that never
//! saw the first.
//!
//! It spawns the unshipped standalone integration host on a root of its own.
//! That host emulates the public gateway greeting while routing to the real
//! Store worker, so product-level workspace calls still cross the gateway
//! boundary. `--root` makes several tests safe to run at once.
//!
//! **It lives in this package rather than beside the code it tests**, for the
//! same reason `onboarding.rs` does: `CARGO_BIN_EXE_koru-store-server` is only set for
//! tests of the package that declares the binary. `koru-workspace` is a
//! dev-dependency here — the second edge pointing this way, and it points this
//! way for a mechanical reason rather than an architectural one.
//!
//! What is deliberately *not* asserted: anything about the folder transport.
//! This is the replacement, tested on its own terms, before anything is asked
//! to depend on it.

mod common;

// Most tests below predate the zero-Profile shell and are about documents, not
// onboarding. Give those fixtures an explicit Profile while keeping production
// launch honest. Tests of the empty catalog call the real functions directly.
mod launch_workspace {
    pub use ::koru_workspace::*;

    pub mod fromstore {
        use std::path::Path;

        use ::koru_workspace::fromstore::StoreLaunch;
        pub use ::koru_workspace::fromstore::{
            OpenedFromStore, create_workspace, rename_workspace, set_workspace_appearance,
        };
        use koru_core::{ProfileId, WorkspaceId};

        fn open_after_explicit_profile(
            segment: &str,
            root: Option<&Path>,
            open: impl Fn() -> koru_store_core::Result<StoreLaunch>,
        ) -> koru_store_core::Result<OpenedFromStore> {
            match open()? {
                StoreLaunch::Open(opened) => Ok(opened),
                StoreLaunch::NeedsProfile => {
                    ::koru_workspace::fromstore::create_local_profile(
                        segment,
                        root,
                        "Test Profile",
                    )?;
                    open()?.into_open().ok_or_else(|| {
                        koru_store_core::Error::new(
                            koru_store_core::Code::Internal,
                            "created Profile was not visible to the next launch",
                        )
                    })
                }
            }
        }

        pub fn open_from_store(
            segment: &str,
            root: Option<&Path>,
        ) -> koru_store_core::Result<OpenedFromStore> {
            open_after_explicit_profile(segment, root, || {
                ::koru_workspace::fromstore::open_from_store(segment, root)
            })
        }

        pub fn open_from_store_preferring(
            segment: &str,
            root: Option<&Path>,
            wanted: Option<WorkspaceId>,
        ) -> koru_store_core::Result<OpenedFromStore> {
            open_after_explicit_profile(segment, root, || {
                ::koru_workspace::fromstore::open_from_store_preferring(segment, root, wanted)
            })
        }

        pub fn open_from_store_preferring_empty(
            segment: &str,
            root: Option<&Path>,
            wanted: Option<WorkspaceId>,
        ) -> koru_store_core::Result<OpenedFromStore> {
            open_after_explicit_profile(segment, root, || {
                ::koru_workspace::fromstore::open_from_store_preferring_empty(segment, root, wanted)
            })
        }

        pub fn open_from_store_profile_preferring_empty(
            segment: &str,
            root: Option<&Path>,
            selected: ProfileId,
            wanted: Option<WorkspaceId>,
        ) -> koru_store_core::Result<OpenedFromStore> {
            ::koru_workspace::fromstore::open_from_store_profile_preferring_empty(
                segment, root, selected, wanted,
            )?
            .into_open()
            .ok_or_else(|| {
                koru_store_core::Error::new(
                    koru_store_core::Code::ProfileNotFound,
                    "selected Profile was not available",
                )
            })
        }
    }
}

use launch_workspace as koru_workspace;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use koru_api_client::Client;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_workspace::store::{StoreBlobs, StoreSession};

fn segment() -> &'static str {
    koru_channel::segment()
}

/// A real store process on a root of its own.
struct Store {
    child: Option<Child>,
    root: PathBuf,
}

impl Store {
    fn start(root: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .arg("serve")
            // A suite starting daemons on throwaway roots must never raise a
            // modal dialog: nothing dismisses it, so the process would never
            // exit and would go on holding its databases and this binary.
            .env("KORU_STORE_NO_DIALOG", "1")
            // `--console` on purpose: without it a store that cannot start puts
            // up a modal dialog, and a test must never do that.
            .arg("--console")
            .arg("--root")
            .arg(root);
        common::configure(&mut command, root);
        let child = command.spawn().expect("spawn koru-store-server");

        let deadline = Instant::now() + Duration::from_secs(30);
        while common::discovery(root).map(|d| Client::new(d).status().is_ok()) != Some(true) {
            assert!(
                Instant::now() < deadline,
                "the store never became reachable"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            child: Some(child),
            root: root.to_path_buf(),
        }
    }

    fn client(&self) -> Client {
        Client::new(common::discovery(&self.root).expect("discover"))
    }

    /// Kill the process, as closing it from the notification area would.
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Start a new one on the same root — a different process, a different
    /// port, and the same databases.
    fn restart(&mut self) {
        self.kill();
        *self = Self::start(&self.root.clone());
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A temp root that is this test's alone.
fn root_for(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "koru-store-roundtrip-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

/// Tick until the store has acknowledged everything this workspace owes it.
///
/// **This is what a test has to wait for before killing the store**, and three
/// tests here quietly did not. A local read — `entries()`, or the document's own
/// rows — is true the instant something is typed and says nothing about whether
/// it left the process; those tests passed only because `publish` used to write
/// to the socket inside the call. With the socket on a thread of its own the
/// bytes may still be in a channel, so killing the store at that point is
/// killing it mid-flight, and the assertion afterwards is about a race.
///
/// The rule it restores is the one `AGENTS.md` states about the vertical slice:
/// assert on **acknowledgements**, never on what was sent. `Workspace::owed`
/// counts what the store has not confirmed, across every open document and the
/// manifest.
fn settle(ws: &mut koru_workspace::Workspace) {
    wait_for("the store to acknowledge everything owed", || {
        ws.tick();
        ws.owed() == 0
    });
}

/// Poll until `f` is true, or fail saying what was being waited for.
///
/// **A liveness deadline, not a budget.** Every caller is waiting for something
/// to *arrive* — through a real store process, over a real socket — and none of
/// them asserts how long that takes. So the number is only here to fail a store
/// that never answers, and it is generous on purpose: this suite starts real
/// processes while the rest of a `cargo test --workspace` run starts dozens
/// more, and ten seconds timed that machine rather than this store.
/// `a_document_created_in_one_window_reaches_another_on_a_tick` failed once in
/// five full-suite runs at ten, and passes alone every time.
///
/// The line to hold: a *liveness* deadline can be as generous as it likes,
/// because nothing is claimed by it. A *performance* budget cannot, which is
/// why `apps/koru/src/perf.rs` keeps tight numbers and is run on its own
/// (`docs/contributing/operations/RUNBOOK.md`) rather than being relaxed until it asserts nothing.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **The seam.** A document written in one `WorkspaceDoc` arrives, through the
/// store, in another that never saw it.
///
/// This is the shape the whole cutover rests on, so it is asserted on *content*
/// — the rows and their text — rather than on bytes having moved. Two replicas
/// agreeing on a byte count would pass while both were empty.
#[test]
fn a_document_written_here_arrives_over_there() {
    let root = root_for("seam");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");
    let document = client
        .create_document(workspace.id, "Roadmap", "page")
        .expect("document");

    // ---- the writer
    let (mut writing, snapshot) =
        StoreSession::join(segment(), Some(&root), workspace.id, document).expect("join");
    let mut doc = if snapshot.is_empty() {
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Acme").expect("create")
    } else {
        WorkspaceDoc::from_snapshot(&snapshot).expect("open the store's snapshot")
    };

    let page = doc
        .insert(Parent::Root, Node::document("Roadmap").unwrap())
        .unwrap();
    doc.insert(
        Parent::Node(page),
        Node::paragraph("Ship the store").unwrap(),
    )
    .unwrap();
    let batch = writing
        .publish(&doc)
        .expect("publish")
        .expect("something to send");

    // **Durability is an answer, not an assumption.** The whole reason for the
    // store is that an acknowledgement means the bytes are on the disk, so the
    // test waits for the ack rather than for a sleep to expire.
    wait_for("the store to call the write durable", || {
        let _ = writing.drain(&mut doc);
        writing.is_durable(batch)
    });
    assert_eq!(writing.unacknowledged(), 0, "nothing left in flight");

    // ---- the reader, which has never seen the writer
    let (_reading, snapshot) =
        StoreSession::join(segment(), Some(&root), workspace.id, document).expect("second join");
    let there = WorkspaceDoc::from_snapshot(&snapshot).expect("open");

    let rows: Vec<String> = there
        .walk()
        .expect("walk")
        .into_iter()
        .filter_map(|(id, _)| there.read(id).ok())
        .filter_map(|n| n.content.as_text().map(str::to_owned))
        .collect();
    assert!(
        rows.iter().any(|r| r == "Ship the store"),
        "the paragraph did not survive the round trip: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r == "Roadmap"),
        "the document node did not survive the round trip: {rows:?}"
    );
}

/// An importer already has a complete Loro snapshot. Registering that page and
/// its bytes is one store operation: a reader joining immediately after the
/// manifest changes must receive the imported content, never an empty page.
#[test]
fn an_imported_document_is_complete_when_it_first_appears() {
    let root = root_for("imported-document");
    let store = Store::start(&root);
    let client = store.client();
    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");

    let mut imported = WorkspaceDoc::create(
        koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid()),
        "Acme",
    )
    .expect("document");
    let page = imported
        .insert(Parent::Root, Node::document("Imported").unwrap())
        .unwrap();
    imported
        .insert(
            Parent::Node(page),
            Node::paragraph("Present in the initial snapshot").unwrap(),
        )
        .unwrap();
    let document = client
        .import_document(
            workspace.id,
            "Imported",
            "page",
            imported.export_snapshot().unwrap(),
        )
        .expect("import");

    let (_, snapshot) = StoreSession::join(segment(), Some(&root), workspace.id, document)
        .expect("join imported document");
    let opened = WorkspaceDoc::from_snapshot(&snapshot).expect("complete initial snapshot");
    let text: Vec<_> = opened
        .walk()
        .unwrap()
        .into_iter()
        .filter_map(|(id, _)| opened.read(id).ok())
        .filter_map(|node| node.content.as_text().map(str::to_owned))
        .collect();
    assert!(
        text.iter()
            .any(|line| line == "Present in the initial snapshot")
    );

    let before = client.documents(workspace.id).unwrap().len();
    assert!(
        client
            .import_document(workspace.id, "Broken", "page", Vec::new())
            .is_err()
    );
    assert_eq!(
        client.documents(workspace.id).unwrap().len(),
        before,
        "a rejected import leaves no manifest row"
    );
}

/// Publishing twice with nothing written in between sends nothing.
///
/// **The trap this guards.** An update covering no operations still carries a
/// header, so it is never an empty `Vec` — a send guarded on `!bytes.is_empty()`
/// would push a frame on every poll, forever, to every peer in the room. The
/// guard has to be a version-vector comparison, and this is what says so.
#[test]
fn publishing_an_unchanged_document_sends_nothing() {
    let root = root_for("idle");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");
    let document = client
        .create_document(workspace.id, "Notes", "page")
        .expect("document");

    let (mut session, _) =
        StoreSession::join(segment(), Some(&root), workspace.id, document).expect("join");
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Acme").unwrap();
    doc.insert(Parent::Root, Node::document("Notes").unwrap())
        .unwrap();

    assert!(
        session.publish(&doc).unwrap().is_some(),
        "the first has work"
    );
    assert!(
        session.publish(&doc).unwrap().is_none(),
        "the second has nothing to say and must stay silent"
    );
}

/// The room grammar, pinned.
///
/// It is wire format shared with the server, whose `Room::parse` is the other
/// half. Spelled out here rather than imported, because importing it would drag
/// axum and tokio into every client — so a test is what keeps the two honest.
#[test]
fn a_room_id_is_spelled_the_way_the_store_reads_it() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    assert_eq!(
        koru_workspace::store::document_room(document),
        format!("document:{}", document.to_storage())
    );
    assert_eq!(
        koru_workspace::store::manifest_room(workspace),
        format!("workspace:{}:manifest", workspace.to_storage())
    );
}

/// **The workspace's list, over the wire.** A document added to the manifest
/// here is listed by the store's own catalogue there — which is what Explore
/// will read once `Workspace` holds a manifest instead of a tree.
///
/// Asserted through `Client::documents`, the SQL mirror, rather than by reading
/// our own manifest back: agreeing with ourselves proves nothing. The mirror is
/// rebuilt from the manifest on every change, so if it lists the document then
/// the store understood what was sent.
#[test]
fn a_document_added_to_the_manifest_is_listed_by_the_store() {
    let root = root_for("manifest");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");

    let (mut session, snapshot) =
        StoreSession::join_manifest(segment(), Some(&root), workspace.id).expect("join manifest");
    let mut manifest =
        koru_store_core::manifest::Manifest::from_snapshot(&snapshot).expect("open the manifest");

    let id = DocumentId::new();
    manifest
        .add_document(id, "Roadmap", "page", None)
        .expect("add");
    let batch = session
        .publish(&manifest)
        .expect("publish")
        .expect("something to send");

    wait_for("the manifest write to be durable", || {
        let _ = session.drain(&mut manifest);
        session.is_durable(batch)
    });

    // The store's own listing, not ours.
    let listed = client.documents(workspace.id).expect("documents");
    assert!(
        listed.iter().any(|e| e.id == id && e.title == "Roadmap"),
        "the store does not list the document the manifest added: {listed:?}"
    );
}

/// **An image, stored and fetched back byte for byte.**
///
/// The path a paste takes: bytes in, an address back, the address recorded on a
/// node, and the bytes readable again from that address alone.
///
/// Asserted on the bytes rather than on the call succeeding, because a blob
/// store that returned the wrong picture would pass a "did it error" test and
/// fail a person.
#[test]
fn a_picture_round_trips_through_the_store() {
    let root = root_for("blobs");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");

    let blobs = StoreBlobs::new(store.client(), workspace.id);
    // A PNG header and some payload — not valid image data, and deliberately
    // not: the store files bytes and must not care what they depict.
    let bytes: Vec<u8> = b"\x89PNG\r\n\x1a\n and then some pixels".to_vec();

    let hash = blobs
        .put(bytes.clone(), "image/png", Some("diagram.png"))
        .expect("put");
    assert_eq!(blobs.get(&hash).expect("get"), bytes, "byte for byte");
    assert!(blobs.contains(&hash));

    // **The address is the content**, so the same bytes twice is one blob.
    let again = blobs
        .put(bytes.clone(), "image/png", Some("diagram.png"))
        .expect("second put");
    assert_eq!(again, hash, "the same bytes have the same address");
    assert_eq!(
        client.blobs(workspace.id).expect("list").len(),
        1,
        "and are stored once"
    );

    // The address is what a document records, and it is the store's spelling
    // rather than the app's old FNV-1a one.
    assert!(
        hash.0.starts_with("b3:"),
        "the app records the store's address, not one of its own: {}",
        hash.0
    );

    let node = koru_core::node::Node::image(hash.clone(), "image/png", Some(4), Some(4))
        .expect("image node");
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Acme").unwrap();
    let id = doc.insert(Parent::Root, node).unwrap();
    assert_eq!(
        doc.blob_hashes().unwrap(),
        vec![hash.0.clone()],
        "the document references exactly the blob that was stored"
    );
    let _ = id;
}

/// A hash this workspace has never seen is an error, not a panic and not empty
/// bytes that would draw as a broken picture.
#[test]
fn an_unknown_blob_is_an_error() {
    let root = root_for("blob-miss");
    let store = Store::start(&root);
    let client = store.client();
    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");

    let blobs = StoreBlobs::new(store.client(), workspace.id);
    let missing = koru_core::node::BlobHash(format!("b3:{}", "0".repeat(64)));
    assert!(blobs.get(&missing).is_err());
    assert!(!blobs.contains(&missing));
}

/// **A `Workspace` opened from the store, listing its documents from the
/// manifest and opening one on demand.**
///
/// The chain the whole cutover rests on, end to end: the store holds the
/// documents, `Workspace::entries` answers from the manifest rather than by
/// walking a tree, and `open_document` fetches one only when it is asked for.
#[test]
fn a_workspace_opens_from_the_store_and_lists_its_documents() {
    let root = root_for("workspace");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");
    let doc = client
        .create_document(workspace.id, "Roadmap", "page")
        .expect("document");

    // Put something in the document, the way an editor would.
    let (mut writing, snapshot) =
        StoreSession::join(segment(), Some(&root), workspace.id, doc).expect("join");
    let mut content = if snapshot.is_empty() {
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Roadmap").unwrap()
    } else {
        WorkspaceDoc::from_snapshot(&snapshot).unwrap()
    };
    let page = content
        .insert(Parent::Root, Node::document("Roadmap").unwrap())
        .unwrap();
    content
        .insert(Parent::Node(page), Node::paragraph("First line").unwrap())
        .unwrap();
    let batch = writing.publish(&content).unwrap().unwrap();
    wait_for("the document to be durable", || {
        let _ = writing.drain(&mut content);
        writing.is_durable(batch)
    });

    // ---- now open the workspace the way the app will
    let koru_workspace = koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid());
    let mut w = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    w.open_from_store(segment(), Some(&root), koru_workspace)
        .expect("open from store");

    // The list comes from the manifest, not from a tree.
    let entries = w.entries();
    assert_eq!(entries.len(), 1, "one document: {entries:?}");
    assert_eq!(entries[0].title, "Roadmap");
    assert_eq!(entries[0].parent, None);
    assert_eq!(entries[0].ordinal, 0);

    // Nothing has been fetched yet — a sidebar costs no document loads.
    assert_eq!(w.active(), None, "no document opened by listing one");

    // ---- open it, and the content is there
    w.open_document(entries[0].id).expect("open document");
    assert_eq!(w.active(), Some(entries[0].id));
    let rows: Vec<String> =
        w.ws.walk()
            .unwrap()
            .into_iter()
            .filter_map(|(id, _)| w.ws.read(id).ok())
            .filter_map(|n| n.content.as_text().map(str::to_owned))
            .collect();
    assert!(
        rows.iter().any(|r| r == "First line"),
        "the document opened without its content: {rows:?}"
    );
}

/// **A document opened from the store knows which workspace it is in.**
///
/// `WorkspaceDoc::create` is the only writer of the `C_META` id, and nothing on
/// the store path calls it: the store hands out an empty `LoroDoc` the app seeds
/// nodes into. So every document the running app opened answered `None` to
/// `id()` — and that is step 0 of every canonical task command's preflight, so
/// `/due` on a plain paragraph refused with "the document carries no workspace
/// id" on real content while every suite built on `create` passed.
///
/// This is the test the other one could not be. The fixture above publishes a
/// document it made with `WorkspaceDoc::create`, which quietly gives it a
/// *random* workspace id and hides the gap; this one creates the document only
/// through the store, exactly as the app does.
#[test]
fn a_document_opened_from_the_store_carries_its_workspace_id() {
    let root = root_for("workspace-id");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");
    client
        .create_document(workspace.id, "Roadmap", "page")
        .expect("document");

    let koru_workspace = koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid());
    let mut w = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    w.open_from_store(segment(), Some(&root), koru_workspace)
        .expect("open from store");
    let entries = w.entries();
    assert_eq!(entries.len(), 1, "one document: {entries:?}");
    w.open_document(entries[0].id).expect("open document");

    assert_eq!(
        w.ws.id(),
        Some(koru_workspace),
        "the open seam stamps the workspace the store named"
    );
    // **And the stamp is not in the undo history.** It is a CRDT op like any
    // other, so stamping after `enable_undo` makes it the document's first
    // undoable step and the person's first Ctrl+Z takes the id back off —
    // silently restoring the exact state this fix exists to end.
    assert!(
        !w.ws.can_undo(),
        "the stamp landed inside the undo history, so Ctrl+Z would remove it"
    );

    // And the consequence that made it a bug report: a due on an ordinary
    // paragraph lands, rather than refusing before it reads the node at all.
    let page =
        w.ws.insert(Parent::Root, Node::document("Roadmap").unwrap())
            .unwrap();
    let row =
        w.ws.insert(Parent::Node(page), Node::paragraph("Ship it").unwrap())
            .unwrap();
    let due =
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse("2026-09-04").unwrap());
    let outcome =
        koru_core::tasks::set_due(&mut w.ws, row, due.clone()).expect("a due on a plain row lands");
    assert_eq!(w.ws.read(row).unwrap().due(), Some(due));
    assert_eq!(
        outcome.task.workspace_id(),
        koru_workspace,
        "and the task is filed under the workspace, not a minted id"
    );
}

/// **An edit made through a store-backed `Workspace` reaches another replica.**
///
/// The write half, which the read test above does not cover: type into the
/// active document, let `tick` carry it, and find it in a second workspace that
/// opened the same document independently.
///
/// This is the assertion that says the app could be pointed at the store — a
/// workspace that could read but not write would pass every earlier test here.
#[test]
fn an_edit_through_a_workspace_reaches_another_replica() {
    let root = root_for("edit");
    let store = Store::start(&root);
    let client = store.client();

    let profile = client
        .create_profile("Tester", None, None, None)
        .expect("profile");
    let workspace = client
        .create_workspace(profile.id, "Acme")
        .expect("workspace");
    let doc = client
        .create_document(workspace.id, "Notes", "page")
        .expect("document");
    let koru_workspace = koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid());
    let koru_doc = koru_core::DocumentId::from_uuid(doc.as_uuid());

    let mut here = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    here.open_from_store(segment(), Some(&root), koru_workspace)
        .expect("open");
    here.open_document(koru_doc).expect("open document");

    // Type, the way an editor does.
    let page = here
        .ws
        .insert(Parent::Root, Node::document("Notes").unwrap())
        .unwrap();
    here.ws
        .insert(Parent::Node(page), Node::paragraph("Typed here").unwrap())
        .unwrap();
    here.tick();

    // ---- a second workspace, which has never seen the first
    let mut there = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    there
        .open_from_store(segment(), Some(&root), koru_workspace)
        .expect("open");

    wait_for("the edit to arrive in the other replica", || {
        there.open_document(koru_doc).ok();
        there.tick();
        there
            .ws
            .walk()
            .map(|w| {
                w.into_iter()
                    .filter_map(|(id, _)| there.ws.read(id).ok())
                    .any(|n| n.content.as_text() == Some("Typed here"))
            })
            .unwrap_or(false)
    });
}

/// Inspecting an empty Store is read-only and returns before any identity or
/// document can be constructed.
#[test]
fn a_launch_against_an_empty_store_needs_a_profile_and_writes_nothing() {
    let root = root_for("launch-empty");
    let store = Store::start(&root);

    let first = ::koru_workspace::fromstore::open_from_store(segment(), Some(&root))
        .expect("open from an empty store");
    assert!(matches!(
        first,
        ::koru_workspace::fromstore::StoreLaunch::NeedsProfile
    ));
    assert!(store.client().profiles().expect("profiles").is_empty());
    assert!(store.client().workspaces().expect("workspaces").is_empty());

    let relaunched =
        ::koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("relaunch");
    assert!(matches!(
        relaunched,
        ::koru_workspace::fromstore::StoreLaunch::NeedsProfile
    ));
    assert!(store.client().profiles().expect("profiles").is_empty());
}

#[test]
fn local_profile_creation_needs_no_agent_or_network_and_waits_for_durability() {
    let root = root_for("local-profile");
    let store = Store::start(&root);

    let outcome = ::koru_workspace::fromstore::create_local_profile_and_open(
        segment(),
        Some(&root),
        "Personal",
    )
    .expect("the local Store acknowledged and opened the Profile");
    let opened = match outcome {
        ::koru_workspace::fromstore::LocalProfileCreation::Open(opened) => opened,
        ::koru_workspace::fromstore::LocalProfileCreation::AcknowledgedButNotOpened {
            error,
            ..
        } => panic!("the acknowledged Profile did not open: {error}"),
    };
    let listed = store.client().profiles().expect("profiles");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        opened.profile.id().map(|id| id.as_uuid()),
        Some(listed[0].id.as_uuid())
    );
    assert!(!opened.has_workspace);
    assert!(opened.profile.workspaces().is_empty());
    assert!(opened.workspace.entries().is_empty());
}

#[test]
fn failed_profile_submission_leaves_the_catalog_empty() {
    let root = root_for("local-profile-failed");
    let store = Store::start(&root);

    ::koru_workspace::fromstore::create_local_profile(segment(), Some(&root), "   ")
        .expect_err("a blank Profile name is refused");
    assert!(store.client().profiles().expect("profiles").is_empty());
    assert!(store.client().workspaces().expect("workspaces").is_empty());
    assert!(matches!(
        ::koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("relaunch"),
        ::koru_workspace::fromstore::StoreLaunch::NeedsProfile
    ));
}

#[test]
fn app_startup_can_show_welcome_without_silently_creating_a_workspace() {
    let root = root_for("launch-welcome");
    let store = Store::start(&root);
    ::koru_workspace::fromstore::create_local_profile(segment(), Some(&root), "Personal")
        .expect("profile durability acknowledgement");

    let first =
        koru_workspace::fromstore::open_from_store_preferring_empty(segment(), Some(&root), None)
            .expect("welcome");
    assert!(!first.has_workspace);
    assert!(first.profile.workspaces().is_empty());
    assert!(first.workspace.entries().is_empty());

    let second =
        koru_workspace::fromstore::open_from_store_preferring_empty(segment(), Some(&root), None)
            .expect("welcome again");
    assert!(!second.has_workspace);
    assert!(second.profile.workspaces().is_empty());
    assert_eq!(store.client().profiles().expect("profiles").len(), 1);
}

#[test]
fn app_startup_shows_welcome_when_every_workspace_is_archived() {
    let root = root_for("launch-archived-only");
    let _store = Store::start(&root);

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root))
        .expect("create the initial workspace");
    let archived = first.current;
    koru_workspace::fromstore::set_workspace_appearance(
        segment(),
        Some(&root),
        archived,
        None,
        None,
        false,
        true,
    )
    .expect("archive");

    let opened = koru_workspace::fromstore::open_from_store_preferring_empty(
        segment(),
        Some(&root),
        Some(archived),
    )
    .expect("welcome");
    assert!(!opened.has_workspace, "an archived row was reopened");
    assert!(opened.workspace.entries().is_empty());
    assert!(
        opened.profile.workspace(archived).unwrap().archived,
        "the manager projection still keeps archived rows"
    );
}

/// A second launch finds what the first one made rather than making more.
///
/// The failure this guards is a launch that creates a workspace every time,
/// which looks fine once and gives you nine workspaces by Friday.
#[test]
fn a_second_launch_reopens_rather_than_creating_again() {
    let root = root_for("launch-again");
    let _store = Store::start(&root);

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("first");
    let second =
        koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("second");

    assert_eq!(first.current, second.current, "the same workspace");
    assert_eq!(
        second.profile.workspaces().len(),
        1,
        "one workspace, not two"
    );
}

/// A launch opens the workspace **on a document** when there is one, so the
/// window has something in it.
#[test]
fn a_launch_opens_on_a_document_when_the_workspace_has_one() {
    let root = root_for("launch-doc");
    let store = Store::start(&root);
    let client = store.client();

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("first");
    let workspace = koru_store_core::ids::WorkspaceId::from_uuid(first.current.as_uuid());
    client
        .create_document(workspace, "Roadmap", "page")
        .expect("document");

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    assert_eq!(opened.workspace.entries().len(), 1);
    assert_eq!(
        opened.workspace.active(),
        Some(opened.workspace.entries()[0].id),
        "the window opens on the document rather than on nothing"
    );
}

/// A manifest entry whose lifecycle row is gone is damaged content, not a
/// Store connection failure. Startup must still return the profile and
/// workspace manager so the person can switch away from or remove it.
#[test]
fn a_launch_survives_a_manifest_entry_with_no_stored_document() {
    let root = root_for("launch-missing-document");
    let mut store = Store::start(&root);
    let client = store.client();

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("first");
    let workspace = WorkspaceId::from_uuid(first.current.as_uuid());
    let document = client
        .create_document(workspace, "Unavailable page", "page")
        .expect("document");

    store.kill();
    {
        let db = rusqlite::Connection::open(
            root.join("workspaces")
                .join(workspace.to_storage())
                .join("workspace.db"),
        )
        .expect("workspace database");
        let id = document.to_storage();
        db.execute("DELETE FROM loro_updates WHERE document_id = ?1", [&id])
            .expect("updates");
        db.execute("DELETE FROM loro_checkpoints WHERE document_id = ?1", [&id])
            .expect("checkpoints");
        assert_eq!(
            db.execute("DELETE FROM loro_documents WHERE id = ?1", [&id])
                .expect("document row"),
            1,
        );
    }
    store.restart();

    let opened = koru_workspace::fromstore::open_from_store_preferring(
        segment(),
        Some(&root),
        Some(first.current),
    )
    .expect("the workspace manager remains available");
    assert_eq!(opened.current, first.current);
    assert_eq!(opened.workspace.entries().len(), 1);
    assert_eq!(
        opened.workspace.active(),
        None,
        "a missing page must not be replaced with an unsaved placeholder",
    );
}

/// **A renamed workspace is still renamed after a restart.**
///
/// The bug this was written for: the rename reached an in-memory `ProfileDoc`
/// that is rebuilt from the store on every launch, so the switcher showed the
/// new name until the window closed and then silently showed the old one again.
/// Nothing failed, and nothing was logged.
///
/// The assertion is deliberately "open it again", not "read it back" — reading
/// back through the same handle would have passed while the bug was live.
#[test]
fn a_renamed_workspace_keeps_its_name_across_a_restart() {
    let root = root_for("rename");
    let _store = Store::start(&root);

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("first");
    assert_eq!(first.profile.workspaces()[0].name, "My Workspace");

    koru_workspace::fromstore::rename_workspace(segment(), Some(&root), first.current, "Mobile")
        .expect("rename");

    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    assert_eq!(
        again.profile.workspaces()[0].name,
        "Mobile",
        "the rename did not survive being reopened"
    );
    assert_eq!(again.current, first.current, "and it is the same workspace");
}

/// The manifest carries the new name too, not just the catalog row.
///
/// Two different facts that must agree: the catalog is what a switcher lists,
/// and the manifest travels with the workspace's own document — so a replica
/// holding the manifest and not the catalog still knows what it is called.
#[test]
fn a_rename_reaches_the_manifest_as_well_as_the_catalogue() {
    let root = root_for("rename-manifest");
    let store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    koru_workspace::fromstore::rename_workspace(segment(), Some(&root), opened.current, "Mobile")
        .expect("rename");

    let workspace = koru_store_core::ids::WorkspaceId::from_uuid(opened.current.as_uuid());
    let client = store.client();
    assert_eq!(
        client
            .workspaces()
            .expect("list")
            .iter()
            .find(|w| w.id == workspace)
            .map(|w| w.name.as_str()),
        Some("Mobile"),
        "the catalogue row"
    );

    let manifest_doc = client.manifest(workspace).expect("manifest id");
    let bytes = client.snapshot(workspace, manifest_doc).expect("snapshot");
    let manifest = koru_store_core::manifest::Manifest::from_snapshot(&bytes).expect("open");
    assert_eq!(
        manifest.name().as_deref(),
        Some("Mobile"),
        "the manifest's own name"
    );
}

/// **Each document has its own undo history, and every opened one has a
/// history at all.**
///
/// Two claims, and the second is the one that silently failed: `enable_undo` is
/// off by default, and `App` calls it once at startup — so before this, the
/// first document could be undone and every document opened afterwards refused
/// Ctrl+Z with no error and no sign of why.
///
/// The first claim is what splitting documents buys: `loro::UndoManager` is
/// bound to a `LoroDoc`, so undoing in one tab cannot reach into another.
#[test]
fn each_document_has_its_own_undo_history() {
    let root = root_for("undo");
    let store = Store::start(&root);
    let client = store.client();

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let workspace = koru_store_core::ids::WorkspaceId::from_uuid(opened.current.as_uuid());
    let a = client.create_document(workspace, "A", "page").expect("a");
    let b = client.create_document(workspace, "B", "page").expect("b");

    let mut w = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let ws = &mut w.workspace;

    let a = koru_core::DocumentId::from_uuid(a.as_uuid());
    let b = koru_core::DocumentId::from_uuid(b.as_uuid());

    ws.open_document(a).expect("open a");
    ws.ws
        .insert(Parent::Root, Node::document("A").unwrap())
        .unwrap();
    assert!(
        ws.ws.can_undo(),
        "a document opened from the store can be undone"
    );

    // The second document — the one that used to have no history at all.
    ws.open_document(b).expect("open b");
    assert!(
        !ws.ws.can_undo(),
        "a freshly opened document has nothing of its own to undo yet"
    );
    ws.ws
        .insert(Parent::Root, Node::document("B").unwrap())
        .unwrap();
    assert!(ws.ws.can_undo(), "and once typed in, it can be undone");

    // Undoing here must not reach into A.
    ws.ws.undo().expect("undo b");
    ws.open_document(a).expect("back to a");
    assert!(
        ws.ws.can_undo(),
        "A still has its own history after B was undone"
    );
}

/// **A store-backed workspace hands out a blob store that works.**
///
/// The gap this closes: `Workspace::blobs` read the *folder* session, which a
/// store-backed workspace does not have — so it returned `None`, and pasting an
/// image logged "self.blobs is None" and did nothing. No error reached the
/// person; the picture simply never appeared.
#[test]
fn a_store_backed_workspace_can_store_and_fetch_a_picture() {
    let root = root_for("ws-blobs");
    let _store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let blobs = opened
        .workspace
        .blobs()
        .expect("a store-backed workspace has somewhere to put pictures");

    let bytes = b"\x89PNG\r\n\x1a\n pixels".to_vec();
    let hash = blobs.put(&bytes).expect("put");
    assert!(
        hash.0.starts_with("b3:"),
        "a store workspace files at the store's address: {}",
        hash.0
    );
    assert_eq!(blobs.get(&hash).expect("get"), bytes, "byte for byte");
}

/// **A document created through the workspace is the workspace's, not a page
/// inside the open one.**
///
/// The distinction that made this worth a round trip: `create_document_after`
/// inserts a `Kind::Document` node into the `WorkspaceDoc` in hand, and in a
/// store-backed workspace that document is the *active one* — so a new page
/// would land inside it, appear correctly in the sidebar, and be somewhere else
/// entirely after a reopen.
#[test]
fn a_created_document_belongs_to_the_workspace() {
    let root = root_for("create-doc");
    let _store = Store::start(&root);

    let mut opened =
        koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let ws = &mut opened.workspace;

    let first = ws.create_document("Roadmap").expect("create");
    // **Listed on a tick, not inside the call.** The document is created over
    // HTTP and the manifest change comes back over the socket, a thread away.
    // This used to read straight back and pass, by accident of the socket write
    // being synchronous.
    wait_for("the new document to be listed", || {
        ws.tick();
        ws.entries().len() == 1
    });
    assert_eq!(ws.entries()[0].id, first);
    assert_eq!(ws.entries()[0].parent, None, "top level, not nested");

    // Open it, then make another while it is active — the case that used to
    // nest.
    ws.open_document(first).expect("open");
    let second = ws.create_document("Notes").expect("create second");
    wait_for("the second to be listed", || {
        ws.tick();
        ws.entries().len() == 2
    });

    let entries = ws.entries();
    assert_eq!(entries.len(), 2);
    assert!(
        entries.iter().all(|e| e.parent.is_none()),
        "both are the workspace's: {entries:?}"
    );
    assert!(entries.iter().any(|e| e.id == second));

    // And it survives a reopen, which is where a nested page would have shown
    // up in the wrong place.
    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let titles: Vec<String> = again
        .workspace
        .entries()
        .into_iter()
        .map(|e| e.title)
        .collect();
    assert_eq!(titles, ["Roadmap", "Notes"]);
}

/// **Renaming a document survives closing everything and starting again.**
///
/// A title is row 0 of the document's own body, and Explore reads the manifest —
/// so without the reconcile the sidebar keeps the old name, and keeps it after a
/// restart, because the manifest was never told.
///
/// Typing has no choke point (twenty editor paths reach a title), so the copy is
/// made on the tick that already runs rather than hooked to an edit.
#[test]
fn renaming_a_document_survives_a_restart() {
    let root = root_for("doc-rename");
    let store = Store::start(&root);

    let id = {
        let mut opened =
            koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
        let ws = &mut opened.workspace;
        let id = ws.create_document("Roadmap").expect("create");
        ws.open_document(id).expect("open");

        // Type over the title, the way the editor does — the root node the
        // create seeded, not a second one. (This test used to insert its own,
        // from when a created document arrived empty.)
        let root_node = ws
            .ws
            .children(Parent::Root)
            .unwrap()
            .into_iter()
            .find(|&n| ws.ws.kind_of(n) == Some(koru_core::Kind::Document))
            .expect("the created document has a root node");
        ws.ws.set_text(root_node, "Q3 Plan").unwrap();

        wait_for("the new title to reach the manifest", || {
            ws.tick();
            ws.entries().iter().any(|e| e.title == "Q3 Plan")
        });
        settle(ws);
        id
    };

    // **Everything closed**, including the store process, then started again.
    drop(store);
    let _store = Store::start(&root);

    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let entry = again
        .workspace
        .entries()
        .into_iter()
        .find(|e| e.id == id)
        .expect("the document is still listed");
    assert_eq!(
        entry.title, "Q3 Plan",
        "the rename did not survive a full restart"
    );
}

/// **Nesting and order survive closing everything.**
///
/// A drag in Explore reorders the *manifest*, not the tree of whatever document
/// happens to be open — so this drives `move_entry` and then restarts the store
/// process to prove the manifest was journalled rather than merely changed in
/// memory.
#[test]
fn nesting_and_order_survive_a_restart() {
    let root = root_for("order");
    let store = Store::start(&root);

    let (a, b, c) = {
        let mut opened =
            koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
        let ws = &mut opened.workspace;
        let a = ws.create_document("A").expect("a");
        let b = ws.create_document("B").expect("b");
        let c = ws.create_document("C").expect("c");
        // **Waited for, not read straight back.** A document is created over
        // HTTP and the manifest change returns over the socket, which is a
        // thread away — so the list catches up on a tick rather than inside the
        // call. It used to be immediate by accident of the socket write being
        // synchronous, which is what made this assertion look safe.
        wait_for("all three to be listed", || {
            ws.tick();
            ws.entries().len() == 3
        });
        assert_eq!(
            ws.entries()
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );

        // Nest C under A, and put B last.
        ws.move_entry(c, Some(a), usize::MAX).expect("nest");
        ws.move_entry(b, None, usize::MAX).expect("reorder");
        settle(ws);
        (a, b, c)
    };

    drop(store);
    let _store = Store::start(&root);

    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let entries = again.workspace.entries();
    let by = |id| entries.iter().find(|e| e.id == id).expect("still listed");

    assert_eq!(by(c).parent, Some(a), "C is still nested under A");
    assert_eq!(by(a).parent, None);
    assert_eq!(by(b).parent, None);
    assert_eq!(
        entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["A", "C", "B"],
        "tree order survived: A, its child C, then B"
    );
}

/// **A new document goes below the page it was made from**, not at the end of
/// the library.
///
/// The regression this pins is the store cutover's: the folder-backed path went
/// through `create_document_after` and the store-backed one registered a
/// document with no position at all, so Ctrl+T from the first page of a hundred
/// put the new one at the bottom. `Session::create_document_after` sends the
/// anchor's parent and `ordinal + 1`, and the store applies it inside the same
/// manifest commit as the registration.
///
/// Both halves are asserted, and they are different claims: the **provisional**
/// row is what the sidebar draws in the milliseconds before the broadcast
/// arrives, and the manifest's own listing is what it draws afterwards. Getting
/// only the second right is a page that appears at the end and hops.
#[test]
fn a_new_document_lands_below_the_one_it_was_made_from() {
    let root = root_for("create-after");
    let _store = Store::start(&root);

    let mut opened =
        koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let ws = &mut opened.workspace;
    let a = ws.create_document("A").expect("a");
    let b = ws.create_document("B").expect("b");
    let c = ws.create_document("C").expect("c");
    wait_for("all three to be listed", || {
        ws.tick();
        ws.entries().len() == 3
    });

    // Made from A, which is two places above the end.
    let mid = ws.create_document_after("Mid", Some(a)).expect("mid");
    assert_eq!(
        ws.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["A", "Mid", "B", "C"],
        "the provisional row is listed where the manifest is about to put it"
    );

    wait_for("the manifest to catch up", || {
        ws.tick();
        ws.entries().len() == 4
    });
    let entries = ws.entries();
    assert_eq!(
        entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["A", "Mid", "B", "C"],
        "and the store put it there too"
    );
    assert_eq!(entries[1].id, mid);
    assert_eq!(entries[1].parent, None, "a sibling of A, not a child of it");

    // A nested anchor keeps its parent: a page made from a child is that
    // child's sibling, not a second top-level document.
    ws.move_entry(c, Some(b), usize::MAX).expect("nest");
    settle(ws);
    let nested = ws.create_document_after("Beside C", Some(c)).expect("new");
    wait_for("the nested one to be listed", || {
        ws.tick();
        ws.entries().len() == 5
    });
    let entries = ws.entries();
    let row = entries.iter().find(|e| e.id == nested).expect("listed");
    assert_eq!(row.parent, Some(b), "under C's parent");
    assert_eq!(
        entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["A", "Mid", "B", "C", "Beside C"]
    );

    // No anchor is the honest fallback — a welcome tab has no page to be next
    // to, and the end of the library is where that goes.
    ws.create_document_after("Last", None).expect("last");
    wait_for("the anchorless one to be listed", || {
        ws.tick();
        ws.entries().len() == 6
    });
    assert_eq!(
        ws.entries().last().expect("something").title,
        "Last",
        "with nothing to be below, it appends"
    );
}

/// **A created document is not empty.**
///
/// `Client::create_document` registers a document and gives it a `LoroDoc` with
/// nothing in it — the store does not know what is inside one, deliberately. A
/// Koru document needs a `Kind::Document` root node, whose text is its title and
/// which is row 0 of the body, plus a line to type into.
///
/// Without them the document opened on nothing: no title row, no line, and
/// nothing for a tab to be over — which is what "the new document did not show"
/// looked like from the outside.
#[test]
fn a_created_document_is_ready_to_type_in() {
    let root = root_for("seeded");
    let store = Store::start(&root);

    let id = {
        let mut opened =
            koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
        let ws = &mut opened.workspace;
        let id = ws.create_document("Roadmap").expect("create");

        // Active, with a root node carrying the title and one empty line.
        assert_eq!(ws.active(), Some(id), "creating one opens it");
        let rows: Vec<(koru_core::Kind, String)> = ws
            .ws
            .walk()
            .unwrap()
            .into_iter()
            .filter_map(|(n, _)| ws.ws.read(n).ok())
            .map(|n| (n.kind, n.content.as_text().unwrap_or("").to_string()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (koru_core::Kind::Document, "Roadmap".to_string()),
                (koru_core::Kind::Paragraph, String::new()),
            ],
            "a title row and a line to type in"
        );
        settle(ws);
        id
    };

    // And the seed was published, not just made locally.
    drop(store);
    let _store = Store::start(&root);
    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let mut ws = again.workspace;
    ws.open_document(id).expect("reopen");
    assert!(
        ws.ws
            .walk()
            .unwrap()
            .into_iter()
            .filter_map(|(n, _)| ws.ws.read(n).ok())
            .any(|n| n.content.as_text() == Some("Roadmap")),
        "the seeded title did not survive a restart"
    );
}

/// **Switching between workspaces keeps both listing their documents.**
///
/// The reported failure: two workspaces each showed documents, and after
/// switching a few times both showed none — and switching back did not recover.
///
/// The cause was `switch_to`, the *folder* path, which clears the manifest on
/// its way. With none, `entries()` falls back to projecting the active
/// document's tree, which lists that page's children — nothing. Once cleared,
/// nothing put a manifest back, so every later switch was empty too.
#[test]
fn switching_between_workspaces_keeps_their_documents() {
    let root = root_for("switch");
    let store = Store::start(&root);
    let client = store.client();

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let owner = first.profile.id().expect("profile id");
    let one = first.current;
    let two = koru_workspace::fromstore::create_workspace(segment(), Some(&root), owner, "Two")
        .expect("second workspace");

    client
        .create_document(
            koru_store_core::ids::WorkspaceId::from_uuid(one.as_uuid()),
            "In One",
            "page",
        )
        .expect("doc in one");
    client
        .create_document(
            koru_store_core::ids::WorkspaceId::from_uuid(two.as_uuid()),
            "In Two",
            "page",
        )
        .expect("doc in two");

    let mut ws = first.workspace;
    // Back and forth several times — the report was that it took a few.
    for _ in 0..3 {
        ws.open_from_store(segment(), Some(&root), one)
            .expect("to one");
        assert_eq!(
            ws.entries()
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            ["In One"],
            "workspace one lost its documents"
        );

        ws.open_from_store(segment(), Some(&root), two)
            .expect("to two");
        assert_eq!(
            ws.entries()
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            ["In Two"],
            "workspace two lost its documents"
        );
    }
}

/// Switching away closes every document room from the old workspace.
///
/// The manifest session was replaced correctly, but document sessions stayed
/// in the map indefinitely. That made a workspace with a previously opened tab
/// impossible to delete: the Store saw this same window as an active user even
/// after it had moved to another workspace.
#[test]
fn switching_workspaces_releases_old_document_rooms_before_delete() {
    let root = root_for("switch-releases-rooms");
    let store = Store::start(&root);
    let client = store.client();

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root))
        .expect("open first workspace");
    let owner = opened.profile.id().expect("profile id");
    let one = opened.current;
    let two = koru_workspace::fromstore::create_workspace(segment(), Some(&root), owner, "Two")
        .expect("second workspace");
    let document = client
        .create_document(
            koru_store_core::ids::WorkspaceId::from_uuid(one.as_uuid()),
            "Open in One",
            "page",
        )
        .expect("document in first workspace");

    let mut workspace = opened.workspace;
    workspace
        .open_from_store(segment(), Some(&root), one)
        .expect("refresh first workspace");
    workspace
        .open_document(koru_core::DocumentId::from_uuid(document.as_uuid()))
        .expect("open first workspace document");
    workspace
        .open_from_store(segment(), Some(&root), two)
        .expect("switch away");

    // **The Store learns of a leave a moment after it is sent.** Switching drops
    // every session, which writes the leaves and joins the socket threads, so this
    // window's side is settled when `open_from_store` returns. The Store reads
    // those frames on its own thread, and a delete that gets there first still
    // counts a connection that has already gone. The app's delete waits a second
    // for exactly this; this waits longer, because a loaded runner is slower than
    // a person. A room that is never released does not drain, and fails below
    // with the Store's own count.
    let old = koru_store_core::ids::WorkspaceId::from_uuid(one.as_uuid());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.delete_workspace(old) {
            Ok(_) => break,
            Err(error)
                if error.code() == koru_store_core::Code::WorkspaceInUse
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                panic!("the old workspace still has a connection owned by this window: {error:?}")
            }
        }
    }
}

/// **A document created in one window appears in another, without being asked.**
///
/// The headless half of "everything updates in realtime". Two `Workspace`s open
/// on one store, as two windows are; one creates a document; the other is only
/// *ticked* — no reopen, no switch, nothing a person would have to do.
///
/// It is asserted here rather than in the shell because a pane needs a window.
/// What the shell adds on top is the push into the pane, and the bug that made
/// this worth writing lived there: `set_entries` was nested inside the `if` that
/// compares the *profile and workspace* lists, so a change to neither never
/// reached Explore. Switching workspaces changes one of them, which is why a
/// switch looked like the cure and why this test does not do one.
#[test]
fn a_document_created_in_one_window_reaches_another_on_a_tick() {
    let root = root_for("realtime");
    // Held, not used: dropping it kills the store process.
    let _store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let workspace = opened.current;
    let mut one = opened.workspace;

    // The second window, on the same workspace.
    let mut two = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    two.open_from_store(segment(), Some(&root), workspace)
        .expect("second window");
    assert_eq!(two.entries().len(), 0, "started with documents");

    one.create_document("Made next door").expect("create");
    assert_eq!(
        one.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["Made next door"],
        "the creating window should not wait for its own socket echo",
    );
    assert!(
        one.take_entries_changed(),
        "the shell needs an immediate reason to refresh Explore"
    );

    // Only ticked. If this needed an `open_from_store` it would be proving the
    // opposite of what it claims.
    wait_for("the other window to hear about it", || {
        two.tick();
        !two.entries().is_empty()
    });
    assert_eq!(
        two.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["Made next door"],
    );

    // ...and a rename reaches it the same way, which is the other half of what
    // the manifest is for. Typed over the title row, as the editor does, so the
    // path under test includes `reconcile_title` rather than a manifest write
    // no keystroke would ever make.
    //
    // The successful HTTP response already supplies a provisional local row.
    // The socket echo replaces it later; Explore and pill routing do not wait.
    let id = one.entries()[0].id;
    one.open_document(id).expect("open it");
    let title_row = one
        .ws
        .children(Parent::Root)
        .unwrap()
        .into_iter()
        .find(|&n| one.ws.kind_of(n) == Some(koru_core::Kind::Document))
        .expect("the created document has a root node");
    one.ws.set_text(title_row, "Renamed next door").unwrap();

    one.tick();
    assert!(
        one.take_entries_changed(),
        "a local title reconcile must refresh pill captions and Explore"
    );
    // **Waited for, because the local title is an eventual property and not an
    // immediate one.** `tick_store` runs `reconcile_title` *before* the manifest
    // session drains, and a manifest title is last-writer-wins by design
    // (ADR-91) — so a remote op still carrying the old name can win that merge
    // and undo the rename until the next tick's reconcile notices the mismatch
    // and writes it again. Asserting after exactly one tick made this pass
    // thousands of times and fail once under a full-suite load, which is the
    // worst possible shape for a test. TECHDEBT 89 is the ordering itself.
    wait_for("the local entry to settle on the new title", || {
        one.tick();
        one.entries()[0].title == "Renamed next door"
    });

    wait_for("the rename to arrive next door", || {
        one.tick();
        two.tick();
        two.entries()
            .first()
            .is_some_and(|e| e.title == "Renamed next door")
    });
}

/// **Typing while the store is down is not lost when it comes back.**
///
/// The reported sequence, exactly: write, stop the store, write more, start the
/// store again. What happened before was nothing at all — a dead socket stayed
/// dead, everything typed afterwards stayed in this process, and closing the
/// window took it.
///
/// Asserted from a **third replica**, opened from the store after the fact and
/// having never seen this process. That is the only version of "it was saved"
/// that means anything: the window's own copy would show both rows whether or
/// not a single byte ever left it.
#[test]
fn typing_while_the_store_is_down_survives_its_return() {
    let root = root_for("outage");
    let mut store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let workspace = opened.current;
    let mut ws = opened.workspace;

    let id = ws.create_document("Outage").expect("create");
    ws.open_document(id).expect("open");
    let root_node = ws
        .ws
        .children(Parent::Root)
        .unwrap()
        .into_iter()
        .find(|&n| ws.ws.kind_of(n) == Some(koru_core::Kind::Document))
        .expect("a root node");
    ws.ws
        .insert(Parent::Node(root_node), Node::paragraph("before").unwrap())
        .expect("insert");
    settle(&mut ws);

    // ---- the store goes away, and typing does not stop
    store.kill();
    ws.ws
        .insert(
            Parent::Node(root_node),
            Node::paragraph("while it was down").unwrap(),
        )
        .expect("insert");
    // Several turns against nothing, as a window would take while somebody types.
    for _ in 0..20 {
        ws.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ws.store_trouble().is_some(),
        "the window did not notice the store had gone"
    );

    // ---- and comes back
    store.restart();
    // **Waited for, not asserted after `settle`.** `settle` waits for
    // `owed() == 0`, and that is true the instant a reconnect rewinds — the
    // rewind clears the in-flight batches, so nothing is owed while the link is
    // still coming up. Asserting here read as "the window never noticed" on any
    // run where the store took a moment longer to answer.
    wait_for("the window to notice the store returned", || {
        ws.tick();
        ws.store_trouble().is_none()
    });
    settle(&mut ws);

    // ---- a replica that never saw this process
    let mut third = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    third
        .open_from_store(segment(), Some(&root), workspace)
        .expect("third replica");
    third.open_document(id).expect("open there");
    let rows: Vec<String> = third
        .ws
        .walk()
        .unwrap()
        .into_iter()
        .filter_map(|(n, _)| third.ws.read(n).ok())
        .filter_map(|n| n.content.as_text().map(str::to_owned))
        .collect();
    assert!(
        rows.iter().any(|r| r == "before"),
        "what was written before the outage is gone: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r == "while it was down"),
        "what was written during the outage never reached the store: {rows:?}"
    );
}

/// **And it is not lost when Koru closes before the store comes back.**
///
/// The sibling of the test above, and the harder half. That one covers a store
/// that returns *while the window is open*, which the reconnect already handled.
/// This one closes Koru first — the case where everything typed lived only in a
/// process that is now gone, and the case that could still cost somebody their
/// afternoon.
///
/// Asserted from a **replica that never saw the process that typed it**, opened
/// after both the window and the store were restarted. Reading the row back
/// through the same handle would pass whether or not a byte ever left it.
#[test]
fn typing_while_the_store_is_down_survives_closing_koru() {
    let root = root_for("outbox");
    let mut store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let workspace = opened.current;
    let mut ws = opened.workspace;

    let id = ws.create_document("Notes").expect("create");
    ws.open_document(id).expect("open");
    let root_node = ws
        .ws
        .children(Parent::Root)
        .unwrap()
        .into_iter()
        .find(|&n| ws.ws.kind_of(n) == Some(koru_core::Kind::Document))
        .expect("a root node");
    ws.ws
        .insert(Parent::Node(root_node), Node::paragraph("before").unwrap())
        .expect("insert");
    // Acknowledged, so the buffer written later starts from a point the store
    // can actually reach back to.
    settle(&mut ws);

    // ---- the store goes away, and typing does not stop
    store.kill();
    ws.ws
        .insert(
            Parent::Node(root_node),
            Node::paragraph("typed with nowhere to send it").unwrap(),
        )
        .expect("insert");
    for _ in 0..20 {
        ws.tick();
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- and *then* Koru closes, which is what the shell does on the way out
    let unconfirmed = ws.unconfirmed();
    assert!(
        !unconfirmed.is_empty(),
        "the window thought everything had reached the store"
    );
    ws.save();
    assert!(
        ws.unconfirmed().not_buffered.is_none(),
        "the work could not be written down: {:?}",
        ws.unconfirmed().not_buffered
    );
    drop(ws);

    // The evidence, on the disk, in the store's own root.
    let outbox = root.join("outbox");
    assert!(
        outbox.exists(),
        "closing with unconfirmed work left no outbox behind"
    );

    // ---- both come back, in the order they would in life
    store.restart();
    let reopened = koru_workspace::fromstore::open_from_store_preferring(
        segment(),
        Some(&root),
        Some(workspace),
    )
    .expect("reopen");
    let mut ws = reopened.workspace;
    ws.open_document(id).expect("open again");
    assert!(
        ws.take_replay_trouble().is_empty(),
        "the buffer could not be put back"
    );
    settle(&mut ws);
    drop(ws);

    // ---- a replica that never saw either process
    let mut third = koru_workspace::Workspace::new(
        WorkspaceDoc::create(koru_core::WorkspaceId::new(), "").unwrap(),
    );
    third
        .open_from_store(segment(), Some(&root), workspace)
        .expect("third replica");
    third.open_document(id).expect("open there");
    let rows: Vec<String> = third
        .ws
        .walk()
        .unwrap()
        .into_iter()
        .filter_map(|(n, _)| third.ws.read(n).ok())
        .filter_map(|n| n.content.as_text().map(str::to_owned))
        .collect();
    assert!(
        rows.iter().any(|r| r == "before"),
        "what was written before the outage is gone: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r == "typed with nowhere to send it"),
        "what was typed while the store was down did not survive the close: {rows:?}"
    );
}

/// The buffer is not left behind once the store has taken it.
///
/// **The half that is easy to skip**, and skipping it means every open replays
/// work the store already has, forever — harmless, invisible, and growing.
#[test]
fn the_outbox_is_emptied_once_the_store_has_the_work() {
    let root = root_for("outbox-pruned");
    let mut store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let workspace = opened.current;
    let mut ws = opened.workspace;
    let id = ws.create_document("Notes").expect("create");
    ws.open_document(id).expect("open");
    let root_node = ws
        .ws
        .children(Parent::Root)
        .unwrap()
        .into_iter()
        .find(|&n| ws.ws.kind_of(n) == Some(koru_core::Kind::Document))
        .expect("a root node");
    settle(&mut ws);

    store.kill();
    ws.ws
        .insert(Parent::Node(root_node), Node::paragraph("owed").unwrap())
        .expect("insert");
    for _ in 0..20 {
        ws.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    ws.save();
    assert!(
        buffered_files(&root) > 0,
        "nothing was buffered, so this proves nothing"
    );

    store.restart();
    settle(&mut ws);

    wait_for("the outbox to be emptied", || {
        ws.tick();
        buffered_files(&root) == 0
    });
    let _ = workspace;
}

/// Every buffered file under a store root, however deep.
fn buffered_files(root: &Path) -> usize {
    fn walk(dir: &Path, found: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().and_then(|e| e.to_str()) == Some("loro") {
                *found += 1;
            }
        }
    }
    let mut found = 0;
    walk(&root.join("outbox"), &mut found);
    found
}

/// **A launch reopens the workspace you were last in, not the newest one.**
///
/// Without a preference the store hands back whichever workspace was *created*
/// last — so making one permanently changes what Koru opens on, and switching
/// back to an older one is forgotten the moment the window closes. That is a
/// daily annoyance rather than a rare one: creating a workspace is exactly when
/// somebody is most likely to go back to the one they were using.
///
/// A remembered id that no longer names one of this profile's workspaces falls
/// back rather than failing, which is what makes deleting the remembered
/// workspace survivable.
#[test]
fn a_launch_reopens_the_workspace_that_was_last_open() {
    let root = root_for("remembered");
    let _store = Store::start(&root);

    let first = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let older = first.current;
    let owner = first.profile.id().expect("profile id");
    let newer = koru_workspace::fromstore::create_workspace(segment(), Some(&root), owner, "Newer")
        .expect("second workspace");
    assert_ne!(older, newer);

    // No preference: the newest wins, which is the behaviour a first run wants.
    let fresh = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    assert_eq!(fresh.current, newer, "a plain open should take the newest");

    // Asked for the older one — the one somebody switched back to.
    let remembered =
        koru_workspace::fromstore::open_from_store_preferring(segment(), Some(&root), Some(older))
            .expect("remembered");
    assert_eq!(
        remembered.current, older,
        "the remembered workspace was not reopened"
    );

    // A workspace that is not this profile's, or no longer exists at all.
    let stranger = koru_core::WorkspaceId::new();
    let fallen_back = koru_workspace::fromstore::open_from_store_preferring(
        segment(),
        Some(&root),
        Some(stranger),
    )
    .expect("fall back");
    assert_eq!(
        fallen_back.current, newer,
        "a stale remembered id should fall back to the newest, not fail"
    );
}

#[test]
fn a_store_profile_switch_opens_that_exact_profile_and_lists_only_store_profiles() {
    let root = root_for("profile-switch");
    let store = Store::start(&root);
    let client = store.client();
    let first = client
        .create_profile("First", Some("work"), None, None)
        .expect("first profile");
    let second = client
        .create_profile("Second", Some("personal"), None, None)
        .expect("second profile");
    let _first_workspace = client
        .create_workspace(first.id, "First Workspace")
        .expect("first workspace");
    let second_workspace = client
        .create_workspace(second.id, "Second Workspace")
        .expect("second workspace");

    let selected = koru_core::ProfileId::from_uuid(second.id.as_uuid());
    let opened = koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        segment(),
        Some(&root),
        selected,
        None,
    )
    .expect("open selected profile");

    assert_eq!(opened.profile.id(), Some(selected));
    assert_eq!(
        opened.current,
        koru_core::WorkspaceId::from_uuid(second_workspace.id.as_uuid())
    );
    assert_eq!(opened.profiles.len(), 2);
    assert!(
        opened
            .profiles
            .iter()
            .all(|profile| profile.folder.is_empty())
    );
    assert_eq!(
        opened
            .profiles
            .iter()
            .find(|profile| profile.id == selected)
            .map(|profile| profile.email.as_str()),
        Some("personal")
    );

    let missing = koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        segment(),
        Some(&root),
        koru_core::ProfileId::new(),
        None,
    )
    .err()
    .expect("a stale profile id must not fall back");
    assert_eq!(
        missing.code(),
        koru_store_core::errors::Code::ProfileNotFound
    );
}

/// **Move to trash is a move, and it survives everything closing.**
///
/// [ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it): losing content is the one
/// unrecoverable bug, so the button labelled *Move to trash* sets a flag and
/// nothing else. The document leaves the library, is listed in the bin, and
/// comes back whole.
///
/// The restart is the part that matters. A flag that lived only in this
/// process's manifest would pass every in-memory assertion and lose the
/// distinction the moment the window closed — which is the same failure as
/// deleting it, arriving a day later.
#[test]
fn trashing_a_document_is_reversible_and_survives_a_restart() {
    let root = root_for("trash");
    let mut store = Store::start(&root);

    let (kept, binned) = {
        let opened =
            koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
        let mut ws = opened.workspace;
        let kept = ws.create_document("Kept").expect("a");
        let binned = ws.create_document("Binned").expect("b");
        wait_for("both to be listed", || {
            ws.tick();
            ws.entries().len() == 2
        });

        ws.trash_document(binned).expect("trash");
        assert_eq!(
            ws.entries()
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            ["Kept"],
            "the trashed document is still in the library"
        );
        assert_eq!(
            ws.trashed_entries()
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            ["Binned"],
            "the trashed document is not in the bin"
        );
        settle(&mut ws);
        (kept, binned)
    };

    // ---- everything closed, including the store process
    store.restart();

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let mut ws = opened.workspace;
    assert_eq!(
        ws.entries().iter().map(|e| e.id).collect::<Vec<_>>(),
        [kept],
        "the bin did not survive: the document is back in the library"
    );
    assert_eq!(
        ws.trashed_entries()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [binned]
    );

    // ---- and it comes back whole
    ws.restore_document(binned).expect("restore");
    assert_eq!(ws.entries().len(), 2, "restoring did not bring it back");
    assert!(ws.trashed_entries().is_empty());

    // ---- emptying the trash is the one that really deletes
    ws.trash_document(binned).expect("trash again");
    assert_eq!(ws.empty_trashed_documents().expect("empty"), 1);
    assert!(ws.trashed_entries().is_empty());
    assert_eq!(ws.entries().len(), 1, "it took the wrong one with it");
    settle(&mut ws);

    store.restart();
    let after = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("last");
    assert_eq!(
        after
            .workspace
            .entries()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [kept],
        "emptying the trash did not survive"
    );
    assert!(after.workspace.trashed_entries().is_empty());
}

/// **A drop naming a document the manifest no longer holds is stale, not
/// broken.**
///
/// Explore draws from a list the shell *pushed*; between that push and the drop
/// the document may have been binned or removed in another window, or its entry
/// may not have arrived here yet. Answering that with a modal is what produced
/// "Couldn't move that document" for a gesture nobody could reproduce.
///
/// `Ok(false)` means *nothing to do*, and the caller's job is to correct the
/// list rather than to interrupt somebody. A real fault still errors.
#[test]
fn moving_a_document_the_manifest_has_lost_is_not_an_error() {
    let root = root_for("stale-move");
    let _store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let mut ws = opened.workspace;
    let real = ws.create_document("Real").expect("create");
    wait_for("it to be listed", || {
        ws.tick();
        !ws.entries().is_empty()
    });

    // A row the pane could plausibly still be holding, naming nothing.
    let ghost = koru_core::DocumentId::new();
    assert_eq!(
        ws.move_entry(ghost, None, 0).expect("not an error"),
        false,
        "a stale row was reported as a fault"
    );
    // ...and the same for a parent that has gone.
    assert_eq!(
        ws.move_entry(real, Some(ghost), 0).expect("not an error"),
        false
    );
    // Onto itself: meaningless rather than faulty.
    assert_eq!(
        ws.move_entry(real, Some(real), 0).expect("not an error"),
        false
    );

    // A move that means something still happens.
    let second = ws.create_document("Second").expect("create");
    wait_for("both to be listed", || {
        ws.tick();
        ws.entries().len() == 2
    });
    assert!(ws.move_entry(second, Some(real), 0).expect("move"));
    assert_eq!(
        ws.entries().iter().find(|e| e.id == second).unwrap().parent,
        Some(real),
        "the real move did not happen"
    );
}

/// **A workspace's icon and colour survive a restart.**
///
/// Reported from a running window: set them, reopen, gone. They were written to
/// the `ProfileDoc` — a *view* rebuilt from the store every launch — to the
/// folder profile document, and to a folder sidecar. Three writes, none of which
/// a store-backed launch reads back. The name survived only because renaming has
/// its own route through the store, which is what made it look arbitrary.
///
/// Asserted through a **fresh open**, since that is the code path that was
/// wrong: the in-memory value was never the problem.
#[test]
fn a_workspaces_icon_and_colour_survive_a_restart() {
    let root = root_for("appearance");
    let mut store = Store::start(&root);

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
    let id = opened.current;
    assert_eq!(opened.profile.workspace(id).unwrap().icon, None);

    koru_workspace::fromstore::set_workspace_appearance(
        segment(),
        Some(&root),
        id,
        Some("\u{1F52C}"),
        Some("purple"),
        true,
        false,
    )
    .expect("set");

    // Everything closed, the store process included.
    store.restart();

    let again = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let entry = again.profile.workspace(id).expect("still listed");
    assert_eq!(
        entry.icon.as_deref(),
        Some("\u{1F52C}"),
        "the icon was lost"
    );
    assert_eq!(
        entry.color.as_deref(),
        Some("purple"),
        "the colour was lost"
    );
    assert!(entry.pinned, "the pin was lost");
    assert!(!entry.archived);

    // **A colour name, not a rendering of one.** Storing `#6B4FC4` would freeze
    // one theme's purple into somebody's workspace for good.
    assert!(
        !entry.color.as_deref().unwrap_or_default().starts_with('#'),
        "a colour value reached storage where a name belongs"
    );

    // ...and clearing them sticks too, which a naive "only write what is set"
    // would quietly refuse.
    koru_workspace::fromstore::set_workspace_appearance(
        segment(),
        Some(&root),
        id,
        None,
        None,
        false,
        true,
    )
    .expect("clear");
    store.restart();
    let cleared = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("last");
    let entry = cleared.profile.workspace(id).expect("still listed");
    assert_eq!(entry.icon, None, "the icon could not be cleared");
    assert_eq!(entry.color, None);
    assert!(!entry.pinned);
    assert!(entry.archived);
}

/// **Save a copy makes a second document, and it survives everything closing.**
///
/// The other way out of the *In Trash* notice: restoring argues with the peer
/// who binned it, copying does not. So the copy has to be a document of the
/// workspace in its own right — in the manifest, with its own bytes in the store
/// — and not a subtree tucked inside the one being read.
///
/// **Asserted on ids as well as on content.** The cheap way to fork a document
/// is to import the source's snapshot into the new one, which carries every
/// `NodeId` across; the two documents would then name the same nodes, and the
/// search index — keyed by `(profile, workspace, node)` — would have two rows
/// fighting over one key. Nothing about the content would look wrong.
#[test]
fn saving_a_copy_makes_a_document_of_its_own() {
    let root = root_for("save-a-copy");
    let mut store = Store::start(&root);

    let (source, copy, source_nodes) = {
        let opened =
            koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("open");
        let mut ws = opened.workspace;
        let source = ws.create_document("Spec").expect("create");
        wait_for("it to be listed", || {
            ws.tick();
            !ws.entries().is_empty()
        });

        // Something to copy, under the document's own root node.
        let doc_root = koru_workspace::document_root(&ws.ws).expect("a seeded root");
        ws.ws
            .insert(
                koru_core::schema::Parent::Node(doc_root),
                Node::paragraph("Ship the store").unwrap(),
            )
            .unwrap();
        let source_nodes: Vec<_> = ws
            .ws
            .walk()
            .expect("walk")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        // The peer bins it under us; copying is the way out that does not argue.
        ws.trash_document(source).expect("trash");
        let copy = ws.copy_document(source, "Spec (copy)").expect("copy");
        assert_ne!(copy, source, "a document of its own, not a second name");

        wait_for("the copy to be listed", || {
            ws.tick();
            ws.entries().iter().any(|e| e.id == copy)
        });
        settle(&mut ws);
        (source, copy, source_nodes)
    };

    // ---- everything closed, including the store process
    store.restart();

    let opened = koru_workspace::fromstore::open_from_store(segment(), Some(&root)).expect("again");
    let mut ws = opened.workspace;
    assert_eq!(
        ws.entries().iter().map(|e| e.id).collect::<Vec<_>>(),
        [copy],
        "the copy is the only thing left in the library"
    );
    assert_eq!(
        ws.trashed_entries()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [source],
        "and the original is still where the peer put it"
    );

    ws.open_document(copy).expect("open the copy");
    let rows: Vec<String> = ws
        .ws
        .walk()
        .expect("walk")
        .into_iter()
        .filter_map(|(id, _)| ws.ws.read(id).ok())
        .filter_map(|n| n.content.as_text().map(str::to_owned))
        .collect();
    assert!(
        rows.iter().any(|r| r == "Ship the store"),
        "the copy lost its content: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r == "Spec (copy)"),
        "the copy did not take the name it was given: {rows:?}"
    );

    let copied: Vec<_> = ws
        .ws
        .walk()
        .expect("walk")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        !copied.iter().any(|id| source_nodes.contains(id)),
        "a node id came across, so the two documents name the same nodes"
    );
}
