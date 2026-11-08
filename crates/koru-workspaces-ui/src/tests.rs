// SPDX-License-Identifier: MPL-2.0

//! The fixtures the controller's suite shares: two Profiles and a workspace
//! row. The surface tests that lived here went with the drawing they tested
//! (ADR-449-onemanagetab); what a press means and why a control is refused
//! are the controller's, and are tested beside it.

use koru_store_core::ids::{ProfileId, WorkspaceId};

use crate::state::Row;

pub(crate) fn owner() -> ProfileId {
    ProfileId::parse("11111111-1111-4111-8111-111111111111").expect("a profile id")
}

/// A second Profile, for a move's destination.
pub(crate) fn elsewhere() -> ProfileId {
    ProfileId::parse("33333333-3333-4333-8333-333333333333").expect("a profile id")
}

pub(crate) fn id(nibble: char) -> WorkspaceId {
    WorkspaceId::parse(&format!("{nibble}{nibble}{nibble}{nibble}{nibble}{nibble}{nibble}{nibble}-2222-4222-8222-222222222222"))
        .expect("a workspace id")
}

pub(crate) fn row(name: &str, at: char) -> Row {
    Row {
        id: id(at),
        owner: owner(),
        name: name.to_owned(),
        icon: None,
        color: None,
        pinned: false,
        archived: false,
        deleted: false,
        position: 0,
        last_opened_at: None,
        authority: crate::Authority::Verified,
        listing_differs: false,
    }
}
