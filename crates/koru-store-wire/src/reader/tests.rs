// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **The tokens a surface branches on, pinned.** A renamed stage would leave
/// every surface that compares it drawing nothing.
#[test]
fn the_tokens_are_pinned() {
    let json = |value: &dyn erased::Json| value.to_json();
    assert_eq!(json(&InstallStage::Downloading), "\"downloading\"");
    assert_eq!(json(&InstallStage::Stopped), "\"stopped\"");
    assert_eq!(json(&CatalogStage::Failed), "\"failed\"");
    assert_eq!(json(&InstallFrom::Settings), "\"settings\"");
    for reason in [
        ReaderUnavailable::NoTrustRoot,
        ReaderUnavailable::NoWorker,
        ReaderUnavailable::WorkerMissing,
        ReaderUnavailable::NotBuilt,
        ReaderUnavailable::NoModel,
        ReaderUnavailable::WillNotLoad,
    ] {
        assert_eq!(json(&reason), format!("\"{}\"", reason.as_str()));
    }
}

/// An empty state — nothing known yet — is a valid answer, and it reads back.
#[test]
fn a_state_round_trips() {
    let mut state = ReaderState {
        offer: Some(ReaderTerms {
            model: "pp-ocrv6-small".into(),
            version: "onnx-2026-11-11".into(),
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
            download_bytes: 26_567_146,
            languages: vec!["und-Latn".into(), "zh".into(), "ja".into()],
        }),
        install: Some(InstallProgress {
            stage: InstallStage::Failed,
            received: 0,
            total: 0,
            code: Some("offline".into()),
        }),
        ..ReaderState::default()
    };
    state
        .counts
        .could_not_be_read_by_reason
        .insert("password".into(), 2);
    let sent = serde_json::to_string(&state).unwrap();
    let back: ReaderState = serde_json::from_str(&sent).unwrap();
    assert_eq!(back, state);
    assert_eq!(
        serde_json::to_value(ReaderState::default()).unwrap()["catalog"]["state"],
        "unread"
    );
}

#[test]
fn only_the_unfinished_stages_are_running() {
    assert!(InstallStage::Downloading.is_running());
    assert!(InstallStage::Stopping.is_running());
    assert!(!InstallStage::Installed.is_running());
    assert!(!InstallStage::Failed.is_running());
    assert!(!InstallStage::Stopped.is_running());
}

/// **An item is ids and tokens and nothing else** — the shape the Settings
/// list resolves names against its own listing from. Pinned field by field,
/// so a name or a path added later is a visible change to this test.
#[test]
fn an_item_carries_ids_and_tokens_only() {
    use koru_store_core::ids::{DocumentId, WorkspaceId};
    let item = ReaderItem {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        hash: "b3:00ff".into(),
        kind: ReaderItemKind::CouldNotBeRead,
        reason: "password".into(),
        pages_done: None,
        offers: crate::readings::ReadingOffers {
            read_all: false,
            read_again: true,
        },
        requested: false,
    };
    let value = serde_json::to_value(&item).unwrap();
    let mut fields: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "document",
            "hash",
            "kind",
            "offers",
            "reason",
            "requested",
            "workspace"
        ]
    );
    assert_eq!(value["kind"], "could_not_be_read");
    // A partly read item says how far it got, as a count.
    let partly = ReaderItem {
        kind: ReaderItemKind::PartlyRead,
        reason: "page-cap".into(),
        pages_done: Some(200),
        ..item.clone()
    };
    let partly_value = serde_json::to_value(&partly).unwrap();
    assert_eq!(partly_value["kind"], "partly_read");
    assert_eq!(partly_value["pages_done"], 200);
    let items = ReaderItems {
        items: vec![item, partly],
        truncated: false,
    };
    let back: ReaderItems = serde_json::from_str(&serde_json::to_string(&items).unwrap()).unwrap();
    assert_eq!(back, items);
    // An answer from before `truncated` existed still reads.
    let old: ReaderItems = serde_json::from_str(r#"{"items":[]}"#).unwrap();
    assert!(!old.truncated);
    // So does an item from before the offers, with nothing offered.
    let older: ReaderItem = serde_json::from_value(serde_json::json!({
        "workspace": WorkspaceId::new(),
        "document": DocumentId::new(),
        "hash": "b3:00",
        "kind": "could_not_be_read",
        "reason": "damaged",
    }))
    .unwrap();
    assert_eq!(older.offers, Default::default());
    assert!(!older.requested && older.pages_done.is_none());
}

/// **Off is its own field, and an older Store's answer reads as on**: a Store
/// from before Plan 3.5 had no off but removal.
#[test]
fn turned_off_defaults_to_on() {
    let old = serde_json::to_value(ReaderState::default())
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
        .into_iter()
        .filter(|(key, _)| key != "turned_off")
        .collect::<serde_json::Map<_, _>>();
    let back: ReaderState = serde_json::from_value(serde_json::Value::Object(old)).unwrap();
    assert!(!back.turned_off);
    let off = ReaderState {
        turned_off: true,
        ..ReaderState::default()
    };
    assert_eq!(serde_json::to_value(&off).unwrap()["turned_off"], true);
}

mod erased {
    pub trait Json {
        fn to_json(&self) -> String;
    }
    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> String {
            serde_json::to_string(self).unwrap()
        }
    }
}
