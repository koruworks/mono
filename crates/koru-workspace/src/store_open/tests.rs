// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{Timestamp, WorkspaceId};

#[test]
fn a_stale_selection_cannot_open_archived_deleted_or_different_content() {
    let id = WorkspaceId::new();
    let expected = koru_core::WorkspaceId::from_uuid(id.as_uuid());
    for archived in [false, true] {
        for deleted in [false, true] {
            let manifest = Manifest::create(id, "Content", Timestamp::from_millis(1)).unwrap();
            manifest.set_archived(archived).unwrap();
            if deleted {
                manifest
                    .set_workspace_deleted_at(Some(Timestamp::from_millis(2)))
                    .unwrap();
            }
            let result = require_live_manifest(&manifest, expected);
            assert_eq!(result.is_ok(), !archived && !deleted);
            if archived || deleted {
                assert_eq!(result.unwrap_err().code(), Code::WorkspaceNotFound);
            }
            assert_eq!(
                require_live_manifest(&manifest, koru_core::WorkspaceId::new())
                    .unwrap_err()
                    .code(),
                Code::CorruptState
            );
        }
    }
}
