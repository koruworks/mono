// SPDX-License-Identifier: MPL-2.0

//! The card the page draws for each thing the Store can say, and the
//! *could not be read* items named from the listing.

use super::*;
use koru_store_wire::reader::{
    CatalogState, InstallProgress, InstalledReader, ReaderItem, ReaderItemKind, ReaderTerms,
};

fn terms() -> ReaderTerms {
    ReaderTerms {
        model: "m".into(),
        version: "1".into(),
        licence: "Apache-2.0".into(),
        attribution: "PaddleOCR".into(),
        download_bytes: 24 * 1024 * 1024,
        languages: vec!["und-Latn".into(), "zh".into(), "ja".into()],
    }
}

fn answer(state: ReaderState) -> Answer {
    Answer {
        state: Some(state),
        ..Answer::default()
    }
}

/// Not downloaded: the real size from the offer, and the languages it reads.
#[test]
fn an_offer_is_a_card_that_can_be_turned_on() {
    let card = row(&answer(ReaderState {
        offer: Some(terms()),
        ..ReaderState::default()
    }))
    .unwrap();
    assert_eq!(
        card.status,
        ReaderStatus::NotDownloaded {
            megabytes: Some(24),
            offline: false,
            ended: None,
        }
    );
    assert_eq!(card.languages, ["und-Latn", "zh", "ja"]);
}

/// Offline, a stopped download, a running one, one that cannot be used, and
/// one that is on — each is its own status.
#[test]
fn every_state_the_store_reports_is_its_own_status() {
    let offline = row(&answer(ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Failed,
            code: Some("offline".into()),
        },
        ..ReaderState::default()
    }))
    .unwrap();
    assert!(matches!(
        offline.status,
        ReaderStatus::NotDownloaded {
            offline: true,
            megabytes: None,
            ..
        }
    ));

    let progress = |stage, received, total| ReaderState {
        offer: Some(terms()),
        install: Some(InstallProgress {
            stage,
            received,
            total,
            code: None,
        }),
        ..ReaderState::default()
    };
    let mib = 1024 * 1024;
    assert_eq!(
        row(&answer(progress(
            InstallStage::Downloading,
            6 * mib,
            24 * mib
        )))
        .unwrap()
        .status,
        ReaderStatus::Downloading {
            stage: ReaderDownloadStage::Downloading,
            received_megabytes: 6,
            total_megabytes: 24,
        }
    );
    assert!(matches!(
        row(&answer(progress(InstallStage::Stopped, 0, 0)))
            .unwrap()
            .status,
        ReaderStatus::NotDownloaded {
            ended: Some(ReaderDownloadEnd::Stopped),
            ..
        }
    ));
    assert!(running(Some(&progress(InstallStage::Verifying, 0, 0))));
    assert!(!running(Some(&progress(InstallStage::Installed, 0, 0))));

    let blocked = row(&answer(ReaderState {
        unavailable: Some(ReaderUnavailable::NotBuilt),
        ..ReaderState::default()
    }))
    .unwrap();
    assert_eq!(
        blocked.status,
        ReaderStatus::Unavailable(ReaderBlocked::NotBuilt)
    );

    let mut on = ReaderState {
        installed: Some(InstalledReader {
            model: "m".into(),
            version: "1".into(),
            download_bytes: 24 * mib,
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
        }),
        ..ReaderState::default()
    };
    on.counts.read = 8_297;
    on.counts.could_not_be_read = 3;
    // The machine's files whose contents are on another device ride along
    // to the card, which names them (OCR Plan 3.3).
    on.counts.not_here = 38;
    let card = row(&answer(on)).unwrap();
    assert_eq!(
        card.status,
        ReaderStatus::On {
            read: 8_297,
            could_not_be_read: 3,
            megabytes: 24,
        }
    );
    assert_eq!(card.not_here, 38);
}

/// **Names come from the listing**: a file by its name, a page holding the
/// picture by its title, an entry the listing did not have unnamed. The
/// token round-trips to the document route.
#[test]
fn items_are_named_from_the_listing_and_lead_to_their_document() {
    let workspace = WorkspaceId::new();
    let (file, page, gone) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    let item = |document, reason: &str| ReaderItem {
        workspace,
        document,
        hash: "b3:00".into(),
        kind: ReaderItemKind::CouldNotBeRead,
        reason: reason.into(),
    };
    let mut state = ReaderState::default();
    state.counts.could_not_be_read = 5;
    let found = Answer {
        state: Some(state),
        items: ReaderItems {
            items: vec![
                item(file, "password"),
                item(page, "unsupported"),
                item(gone, "damaged"),
            ],
            truncated: false,
        },
        names: HashMap::from([
            (
                (workspace, file),
                ("Lease.pdf".to_owned(), "file".to_owned()),
            ),
            (
                (workspace, page),
                ("Site visit".to_owned(), "page".to_owned()),
            ),
        ]),
    };
    let card = row(&found).unwrap();
    assert_eq!(card.items.len(), 3);
    assert_eq!(card.items[0].name.as_deref(), Some("Lease.pdf"));
    assert!(!card.items[0].picture);
    assert_eq!(card.items[0].reason, "password");
    assert_eq!(card.items[1].name.as_deref(), Some("Site visit"));
    assert!(card.items[1].picture, "a page holds the picture");
    assert_eq!(card.items[2].name, None);
    assert_eq!(card.more, 2, "the count is the whole number");

    let target = item_target(&card.items[0].key).expect("a route");
    assert_eq!(
        target,
        koru_workspace_host::NavigationTarget::Document {
            document: koru_core::DocumentId::from_uuid(file.as_uuid()),
            node: None,
        }
    );
    assert_eq!(item_target("not a key"), None);
}

/// The card asks again only while the catalog is being read and the size is
/// unknown; an offer in hand, an installed reader or a blocked one settle it.
#[test]
fn only_a_catalog_being_read_is_asked_about_again() {
    let reading = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Reading,
            code: None,
        },
        ..ReaderState::default()
    };
    assert!(catalog_pending(Some(&reading)));
    assert!(!catalog_pending(Some(&ReaderState {
        offer: Some(terms()),
        ..reading.clone()
    })));
    assert!(!catalog_pending(Some(&ReaderState {
        unavailable: Some(ReaderUnavailable::NoWorker),
        ..reading.clone()
    })));
    assert!(!catalog_pending(None));
}

/// A Store that did not answer draws no card change.
#[test]
fn no_answer_is_no_card() {
    assert_eq!(row(&Answer::default()), None);
}

/// Queued refreshes are one refresh, after every press.
#[test]
fn queued_refreshes_collapse_and_presses_are_kept() {
    assert_eq!(
        dedup_refreshes(vec![Command::Refresh, Command::TurnOn, Command::Refresh]),
        [Command::TurnOn, Command::Refresh]
    );
    assert_eq!(dedup_refreshes(vec![Command::Stop]), [Command::Stop]);
}
