// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{Timestamp, Workspace};

fn source() -> Workspace {
    Workspace::new(ProfileId::new(), "Workspace", Timestamp::from_millis(1)).unwrap()
}

#[test]
fn committed_lifecycle_overrides_stale_listing_and_restoration_is_not_trash() {
    for archived in [false, true] {
        for deleted in [false, true] {
            let mut workspace = source();
            workspace.archived = !archived;
            workspace.deleted_at = (!deleted).then_some(Timestamp::from_millis(1));
            let row = overlay(WorkspaceAuthority {
                workspace,
                listing_differs: true,
                authority: WorkspaceAuthorityState::Read {
                    archived,
                    deleted_at: deleted.then_some(Timestamp::from_millis(2)),
                    pinned: true,
                    applied: true,
                },
            });
            assert_eq!(row.archived, archived);
            assert_eq!(row.deleted, deleted);
            assert!(row.pinned);
            assert_eq!(row.authority, Authority::Verified);
            assert!(
                !row.navigation_ready(),
                "required projection has not settled"
            );
            assert!(row.authority_reason(koru_i18n::Lang::En).is_some());
        }
    }
}

#[test]
fn only_applied_verified_settled_live_content_is_navigable() {
    for applied in [false, true] {
        let row = overlay(WorkspaceAuthority {
            workspace: source(),
            listing_differs: false,
            authority: WorkspaceAuthorityState::Read {
                archived: false,
                deleted_at: None,
                pinned: false,
                applied,
            },
        });
        assert_eq!(row.navigation_ready(), applied);
    }
    let row = overlay(WorkspaceAuthority {
        workspace: source(),
        listing_differs: false,
        authority: WorkspaceAuthorityState::Unreadable {
            cause: "corrupt_state".into(),
            recovery: None,
        },
    });
    assert!(!row.navigation_ready());
    assert!(
        matches!(row.authority, Authority::Uncertain { cause, .. } if cause == "corrupt_state")
    );
    assert!(!row_of(source()).navigation_ready());
}
