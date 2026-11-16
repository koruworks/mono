// SPDX-License-Identifier: MPL-2.0

//! **`koru-chats-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-chats-ui";

const LIB: &str = "crates/koru-chats-ui/src/lib.rs";
const STATUS: &str = "crates/koru-chats-ui/src/status.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: side effects: new(root) starts a reader thread that dials
    // the gateway (skipped under cfg(test) and the no-reader feature);
    // without_reader() builds it inert, documented for tests and for a host
    // that feeds it with take_listing
    record(
        "C-CHATS-LIST",
        "ChatsSurface",
        Key::UiLabChatsListName,
        Key::UiLabChatsListPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_chats_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_chats_ui::ChatsSurface"),
            painter("koru_chats_ui::ChatsSurface::new"),
            painter("koru_chats_ui::ChatsSurface::without_reader"),
            painter("koru_chats_ui::ChatsSurface::draw"),
            support("koru_chats_ui::ChatsSurface::layout"),
            support("koru_chats_ui::ChatsSurface::row_parts"),
            support("koru_chats_ui::ChatsSurface::row_at"),
            support("koru_chats_ui::ChatsSurface::mouse_down"),
            support("koru_chats_ui::ChatsSurface::key"),
            support("koru_chats_ui::ChatsSurface::take_listing"),
            support("koru_chats_ui::ChatsSurface::accessible_name"),
            support("koru_chats_ui::Row"),
            support("koru_chats_ui::RowParts"),
            support("koru_chats_ui::RowButton"),
            support("koru_chats_ui::Notice"),
            support("koru_chats_ui::Press"),
            support("koru_chats_ui::Load"),
            support("koru_chats_ui::Listing"),
            support("koru_chats_ui::Focus"),
            support("koru_chats_ui::ChatStep"),
            support("koru_chats_ui::sort_for_list"),
            support("koru_chats_ui::MODE_ICON"),
            support("koru_chats_ui::text::meta_line"),
            support("koru_chats_ui::text::who_answered"),
        ],
    ),
    // Construction: pure
    record(
        "H-CHAT-STATUS",
        "ChatStatus",
        Key::UiLabChatStatusName,
        Key::UiLabChatStatusPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_chats_ui::status", STATUS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support_with(
                "koru_chats_ui::status::ChatStatus",
                &[values(
                    Key::UiLabControlState,
                    "koru_chats_ui::status::ChatStatus",
                    STATUS,
                    Declared::Enum,
                    &[
                        literal("Idle"),
                        literal("Working"),
                        literal("NeedsYou"),
                        literal("Unread"),
                    ],
                )],
            ),
            support("koru_chats_ui::status::ChatStatus::of"),
            support("koru_chats_ui::status::ChatStatus::mark"),
            support("koru_chats_ui::status::ChatStatus::spoken"),
        ],
    ),
];
