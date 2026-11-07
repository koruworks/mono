// SPDX-License-Identifier: MPL-2.0

//! **Two clients, one gateway, no poller.**
//!
//! The unit tests prove each half: `koru-api-gateway-core` proves the Store
//! publishes a fact after it commits, and `koru-api-client` proves the helper
//! classifies a name and holds a flag. Neither proves the thing R3 actually
//! asks for — that a change made by *one* window becomes an invalidation in
//! *another* without either of them asking. That needs a real gateway process,
//! two real credentials and a real SSE stream, which is what this package has.
//!
//! Nothing here waits on a timer for the answer to become true. Each wait is
//! bounded and fails loudly, because a test that sleeps and then asserts is a
//! test that passes on a fast machine.
//!
//! The real gateway process is `koru-agent.exe --worker gateway`, started by
//! [`gateway_process`]; the composition it drives is the one the retired
//! standalone host drove.

mod gateway_process;

use std::path::Path;
use std::time::{Duration, Instant};

use koru_api_client::{CatalogChange, Client};

use gateway_process::Gateway;

/// Long enough that a loaded CI machine's process start, discovery, SSE
/// connect and journal append all fit; short enough that a broken pipeline
/// fails the run rather than hanging it.
const DELIVERY: Duration = Duration::from_secs(20);

fn connect(root: &Path, product: &str, scopes: Vec<String>) -> Client {
    let hello = koru_api_discovery::ClientHello {
        product: product.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        segment: koru_channel::segment().into(),
        api_version: koru_api_wire::API_VERSION,
        requested_scopes: scopes,
        grant: None,
    };
    let found = koru_api_discovery::connect_at(koru_channel::segment(), root, &hello)
        .expect("connect to the running gateway");
    Client::from_gateway(found).expect("build gateway client")
}

fn start(root: &Path) -> (Gateway, Client) {
    let mut gateway = Gateway::start(root);
    let found = gateway.published(&gateway_process::hello("gateway-catalog-integration"));
    let client = Client::from_gateway(found).expect("build gateway client");
    (gateway, client)
}

/// Wait for a condition the *stream* is supposed to make true. Polling here is
/// the test harness's, not the product's: the window under test holds one SSE
/// subscription and asks nothing of the gateway between facts.
fn within(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + DELIVERY;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} did not happen within {DELIVERY:?}");
}

/// The whole of R3's invalidation contract, end to end.
///
/// Client A is the window that changes something. Client B is every other
/// window: it subscribes once, never polls the catalog, and finds its menus
/// marked stale — the right menu, and only when something actually committed.
#[test]
fn a_second_client_is_invalidated_by_the_first_clients_mutation() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, a) = start(root.path());
    let b = connect(root.path(), "second-window", Vec::new());
    assert!(
        b.events().available(),
        "the gateway opened no events worker; there is nothing to subscribe to"
    );

    // B's whole subscription: one call, one background thread, no cache.
    let watch = b.events().watch_catalog(|_| {}).expect("watch the catalog");
    let flags = watch.flags();
    assert!(!flags.is_stale(CatalogChange::Profiles));
    assert!(!flags.is_stale(CatalogChange::Workspaces));

    // A creates a profile.
    let profile = a
        .store()
        .create_profile("Ada", None, None, Some("en"))
        .unwrap();
    within("the profile fact reached the second client", || {
        flags.is_stale(CatalogChange::Profiles)
    });
    assert!(
        flags.take(CatalogChange::Profiles),
        "opening the menu clears the flag and refetches once"
    );
    assert!(
        !flags.take(CatalogChange::Profiles),
        "and opening it again with nothing in between refetches nothing"
    );

    // A creates a workspace: the *other* half goes stale, and profiles stay
    // clean. A banner that refetched both would be a poller with two steps.
    let workspace = a.store().create_workspace(profile.id, "Notes").unwrap();
    within("the workspace fact reached the second client", || {
        flags.is_stale(CatalogChange::Workspaces)
    });
    assert!(!flags.is_stale(CatalogChange::Profiles));
    assert!(flags.take(CatalogChange::Workspaces));

    // And the fact is *committed* before it is announced: by the time B is
    // told, B's own fetch already sees the change. This is the ordering the
    // whole design rests on — an event is a fact, so a consumer that reacts to
    // one never races the mutation it reports.
    a.store()
        .rename_workspace(workspace.id, "Notebook")
        .unwrap();
    within("the rename reached the second client", || {
        flags.is_stale(CatalogChange::Workspaces)
    });
    let seen = b.store().workspaces().unwrap();
    assert_eq!(
        seen.iter().find(|w| w.id == workspace.id).unwrap().name,
        "Notebook"
    );
    flags.take(CatalogChange::Workspaces);

    // **The removals are not here any more, and their absence is the point.**
    // Since SP-8a, archiving, soft-deleting and restoring take
    // `workspace.destroy`, which is granted only to a caller the Agent vouched
    // for at admission — and a test binary can never be one, because it is not
    // the `koru` beside the Agent. That is not a gap in the coverage; it is the
    // mechanism working. Their facts and their ordering are asserted in
    // process, where a vouched token can be minted, by
    // `every_committed_catalog_mutation_publishes_its_fact_once`.
    //
    // What this asserts instead is the bypass being closed through the real
    // gateway: an ordinary credential holding the whole default set cannot
    // reach the Store's removal routes.
    for refused in [
        a.store()
            .set_workspace_appearance(workspace.id, None, None, false, true)
            .err(),
        a.store().delete_workspace(workspace.id).err(),
        a.store().restore_workspace(workspace.id).err(),
    ] {
        let refused = refused.expect("an ordinary credential removed a workspace");
        assert_eq!(refused.code, koru_store_core::errors::Code::Forbidden);
    }

    a.store()
        .reorder_workspaces(profile.id, &[workspace.id])
        .unwrap();
    within("the reorder reached the second client", || {
        flags.is_stale(CatalogChange::Workspaces)
    });

    watch.close();
}

/// **What gates the catalog, asserted through the real gateway.**
///
/// A credential that asked for `store.read_documents` alone reads both lists —
/// including `?include_deleted=true`, which a workspace manager needs — and
/// changes nothing. The writes below now fall into two groups rather than one:
/// eleven take `store.manage_workspaces` and are ordinary local work; the
/// three that remove a workspace take `workspace.destroy`, which is granted
/// only to a caller the Agent vouched for. A read-only credential is refused
/// by both, which is what this asserts; which of the two refused it is
/// asserted in process, where every token kind can be minted.
#[test]
fn a_reading_credential_reads_the_catalog_and_moves_none_of_it() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, owner) = start(root.path());
    let profile = owner
        .store()
        .create_profile("Ada", None, None, Some("en"))
        .unwrap();
    let workspace = owner.store().create_workspace(profile.id, "Notes").unwrap();

    let reader = connect(
        root.path(),
        "reader",
        vec!["store.read_documents".to_owned()],
    );
    reader.store().profiles().unwrap();
    reader.store().workspaces().unwrap();
    reader.store().workspaces_including_deleted().unwrap();

    let refusals: Vec<(&str, koru_store_core::errors::Error)> = vec![
        (
            "create_profile",
            reader
                .store()
                .create_profile("Grace", None, None, None)
                .unwrap_err(),
        ),
        (
            "rename_profile",
            reader
                .store()
                .rename_profile(profile.id, "Grace")
                .unwrap_err(),
        ),
        (
            "set_profile_appearance",
            reader
                .store()
                .set_profile_appearance(profile.id, Some("star"), None)
                .unwrap_err(),
        ),
        (
            "reorder_workspaces",
            reader
                .store()
                .reorder_workspaces(profile.id, &[workspace.id])
                .unwrap_err(),
        ),
        (
            "create_workspace",
            reader
                .store()
                .create_workspace(profile.id, "Other")
                .unwrap_err(),
        ),
        (
            "rename_workspace",
            reader
                .store()
                .rename_workspace(workspace.id, "Other")
                .unwrap_err(),
        ),
        (
            "set_workspace_appearance",
            reader
                .store()
                .set_workspace_appearance(workspace.id, None, None, true, true)
                .unwrap_err(),
        ),
        (
            "delete_workspace",
            reader.store().delete_workspace(workspace.id).unwrap_err(),
        ),
        (
            "restore_workspace",
            reader.store().restore_workspace(workspace.id).unwrap_err(),
        ),
    ];
    for (what, error) in refusals {
        assert_eq!(
            error.code(),
            koru_store_core::errors::Code::Forbidden,
            "{what} was not refused for want of a scope: {error}"
        );
    }

    // The refusals changed nothing, and — because an event is a fact — said
    // nothing either. The catalog is exactly as the owner left it.
    assert_eq!(owner.store().profiles().unwrap().len(), 1);
    let workspaces = owner.store().workspaces().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].name, "Notes");
    assert!(!workspaces[0].archived);
    assert!(workspaces[0].deleted_at.is_none());
}

/// **The per-subject activity read answers through the host process.**
///
/// The route arrived with dev's Tasks work and is served by the events worker
/// the Gateway opens, behind the Gateway's own bearer and scope — not by the
/// Store fallback and not by a separate daemon. So it is a property of *this
/// composition*, and a rehost that forgot to open the events worker, or that
/// let `/v1/events/*` fall through to the Store, would answer 404 or 503 here
/// while every in-process unit test still passed.
///
/// One event, one subject, read back by that subject. The pane's semantics are
/// pinned where the worker lives; what is pinned here is that the answer comes
/// out of the host at all.
#[test]
fn the_activity_read_answers_for_one_subject_through_the_host() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, client) = start(root.path());
    assert!(
        client.events().available(),
        "the host published no events capability"
    );

    let task = koru_events::catalog::TaskSubject {
        workspace: uuid::Uuid::new_v4(),
        document: uuid::Uuid::new_v4(),
        node: uuid::Uuid::new_v4(),
    };
    let other = koru_events::catalog::TaskSubject {
        workspace: task.workspace,
        document: task.document,
        node: uuid::Uuid::new_v4(),
    };
    client
        .events()
        .send(koru_events::catalog::TaskCreated {
            task,
            reason: koru_events::catalog::TaskCreatedReason::Explicit,
            node_kind: koru_events::catalog::TaskNodeKind::Task,
        })
        .expect("the host accepted a task event");
    client
        .events()
        .send(koru_events::catalog::TaskCreated {
            task: other,
            reason: koru_events::catalog::TaskCreatedReason::Explicit,
            node_kind: koru_events::catalog::TaskNodeKind::Task,
        })
        .expect("the host accepted a second task event");

    let page = client
        .events()
        .activity(koru_events::SubjectKind::Task, &task.subject().id, None, 10)
        .expect("the host answered the activity read");
    // Addressed by subject, so the second task's event is not in this page —
    // the difference between an index seek and a journal scan.
    assert_eq!(page.entries.len(), 1, "{:?}", page.entries);
    assert_eq!(page.entries[0].name.as_str(), "task.created");
    assert_eq!(page.entries[0].reason(), Some("explicit"));
    assert!(
        page.next_after >= page.entries[0].seq,
        "the page did not answer a usable cursor"
    );
}
