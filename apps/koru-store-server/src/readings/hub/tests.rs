// SPDX-License-Identifier: MPL-2.0

use super::*;

fn counts(total: u64, unread: u64, needs_reader: u64, not_here: u64) -> ScopeCounts {
    ScopeCounts {
        total,
        unread,
        needs_reader,
        not_here,
    }
}

/// A desktop hub: an installer and a coordinator, a worker built with
/// recognition, two workspaces counted.
fn desktop() -> (Arc<ReaderHub>, WorkspaceId, WorkspaceId) {
    let hub = ReaderHub::new();
    hub.set_installing_here(false);
    hub.attach_coordinator();
    hub.publish_worker(WorkerKnowledge {
        built: Some(true),
        ..WorkerKnowledge::default()
    });
    let (five, many) = (WorkspaceId::new(), WorkspaceId::new());
    hub.publish_counts(
        [
            (five, counts(5, 5, 5, 0)),
            (many, counts(9_000, 1_200, 800, 38)),
        ]
        .into(),
        MachineReadingCounts::default(),
    );
    (hub, five, many)
}

/// **A workspace with five pictures never shows another workspace's number**
/// (EXPERIENCE §3): the counts are the query's workspaces' and nobody else's.
#[test]
fn counts_for_one_workspace_never_include_another() {
    let (hub, five, many) = desktop();
    let alone = hub.reading_counts(Some(&[five]), &[]).unwrap();
    assert_eq!(
        (
            alone.total,
            alone.unread,
            alone.needs_reader,
            alone.not_here
        ),
        (5, 5, 5, 0)
    );
    let both = hub.reading_counts(Some(&[five, many]), &[]).unwrap();
    assert_eq!(both.unread, 1_205);
    assert_eq!(both.not_here, 38);
    // Every workspace, less one excluded: the other's numbers only.
    let rest = hub.reading_counts(None, &[many]).unwrap();
    assert_eq!(rest.total, 5);
    // A workspace nothing was counted in says zero, not somebody else's.
    let empty = hub
        .reading_counts(Some(&[WorkspaceId::new()]), &[])
        .unwrap();
    assert_eq!((empty.total, empty.unread, empty.offer), (0, 0, false));
}

/// Nothing counted on this host (a phone, a host with no coordinator): no
/// counts at all, which a surface reads as "say nothing".
#[test]
fn a_host_that_counts_nothing_says_nothing() {
    let hub = ReaderHub::new();
    assert_eq!(hub.reading_counts(None, &[]), None);
    assert_eq!(hub.unavailable(), Some(ReaderUnavailable::NoWorker));
}

/// **The offer**: something in scope waits for the reader on this computer,
/// it is not installed, nobody said *Not now*, and nothing says this computer
/// could not use it.
#[test]
fn the_offer_is_made_only_when_it_could_be_taken() {
    let (hub, five, _) = desktop();
    assert!(hub.reading_counts(Some(&[five]), &[]).unwrap().offer);

    hub.set_declined(true);
    assert!(
        !hub.reading_counts(Some(&[five]), &[]).unwrap().offer,
        "a Not now silences it"
    );
    hub.set_declined(false);

    // A worker built without recognition: the download would be useless.
    hub.publish_worker(WorkerKnowledge {
        built: Some(false),
        ..WorkerKnowledge::default()
    });
    assert!(!hub.reading_counts(Some(&[five]), &[]).unwrap().offer);
    assert_eq!(hub.unavailable(), Some(ReaderUnavailable::NotBuilt));
    hub.publish_worker(WorkerKnowledge {
        built: Some(true),
        ..WorkerKnowledge::default()
    });

    // Nothing waiting for the reader: no line.
    let nothing_waits = WorkspaceId::new();
    hub.publish_counts(
        [(nothing_waits, counts(3, 1, 0, 1))].into(),
        MachineReadingCounts::default(),
    );
    assert!(
        !hub.reading_counts(Some(&[nothing_waits]), &[])
            .unwrap()
            .offer
    );
}

/// On a stable build with no model-signing key, nothing is offered and the
/// catalog is never fetched.
#[test]
fn a_stable_build_offers_nothing_and_fetches_nothing() {
    let (hub, five, _) = desktop();
    hub.set_installing_here(true);
    assert_eq!(hub.unavailable(), Some(ReaderUnavailable::NoTrustRoot));
    assert!(!hub.reading_counts(Some(&[five]), &[]).unwrap().offer);
    assert!(!hub.want_catalog(0), "no catalog read for nothing");
    assert!(hub.next_command(std::time::Duration::ZERO).is_none());
}

/// The catalog is asked for once, and again only a while after a failure.
#[test]
fn the_catalog_is_read_once_and_retried_after_a_while() {
    let (hub, _, _) = desktop();
    assert!(hub.want_catalog(0));
    assert!(matches!(
        hub.next_command(std::time::Duration::ZERO),
        Some(Command::FetchCatalog)
    ));
    assert!(!hub.want_catalog(1), "already reading");
    hub.set_catalog(
        CatalogState {
            state: CatalogStage::Failed,
            code: Some("offline".into()),
        },
        None,
        10,
    );
    assert!(!hub.want_catalog(10 + CATALOG_RETRY_MILLIS - 1));
    assert!(hub.want_catalog(10 + CATALOG_RETRY_MILLIS));
    // A catalog with nothing this build can use: the reason, plainly.
    hub.set_catalog(
        CatalogState {
            state: CatalogStage::Ready,
            code: None,
        },
        None,
        20,
    );
    assert_eq!(hub.unavailable(), Some(ReaderUnavailable::NoModel));
}

/// The state a surface reads carries what it needs and asks the coordinator,
/// once, to learn what the worker can do when nobody has asked yet.
#[test]
fn reading_the_state_asks_for_a_probe_once() {
    let hub = ReaderHub::new();
    hub.set_installing_here(false);
    hub.attach_coordinator();
    let state = hub.state();
    assert_eq!(state.installed, None);
    assert!(!state.offer_declined);
    assert!(hub.take_probe_wanted());
    assert!(!hub.take_probe_wanted());
}
