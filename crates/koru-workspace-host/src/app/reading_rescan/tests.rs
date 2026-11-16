// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{ids::DocumentId, model::Timestamp};

fn file() -> DocumentEntry {
    DocumentEntry {
        id: DocumentId::new(),
        parent: None,
        ordinal: 0,
        title: "fixture".into(),
        kind: koru_core::explore::KIND_FILE.into(),
        mime: Some("application/pdf".into()),
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

#[test]
fn files_ocr_rescan_covers_live_files_deduplicates_and_continues_after_failure() {
    let first = file();
    let duplicate = file();
    let unavailable = file();
    let broken = file();
    let last = file();
    let mut parent = file();
    parent.kind = "page".into();
    parent.trashed_at = Some(Timestamp::from_millis(1));
    let mut hidden = file();
    hidden.parent = Some(parent.id);
    let mut trashed = file();
    trashed.trashed_at = Some(Timestamp::from_millis(1));
    let hash =
        |digit: char| BlobHash::parse(&format!("b3:{}", digit.to_string().repeat(64))).unwrap();
    let mut requested = Vec::new();
    let summary = enqueue(
        &[
            first.clone(),
            duplicate.clone(),
            unavailable.clone(),
            broken.clone(),
            last.clone(),
            parent,
            hidden,
            trashed,
        ],
        |entry| {
            if entry.id == broken.id {
                return Err(());
            }
            if entry.id == first.id || entry.id == duplicate.id {
                return Ok(hash('a'));
            }
            if entry.id == unavailable.id {
                return Ok(hash('b'));
            }
            assert_eq!(entry.id, last.id, "only live file entries are resolved");
            Ok(hash('c'))
        },
        |hash_value| {
            requested.push(hash_value.clone());
            Ok(*hash_value != hash('b'))
        },
    );
    assert_eq!(requested, vec![hash('a'), hash('b'), hash('c')]);
    assert_eq!(
        summary,
        Summary {
            queued: 2,
            skipped: 2,
            failed: 1
        }
    );
}
