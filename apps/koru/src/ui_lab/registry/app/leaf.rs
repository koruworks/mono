// SPDX-License-Identifier: MPL-2.0

//! **`apps/koru/src/leaf.rs`**: what a document tab bottoms out at — the
//! workspace app's implementation of both `koru_editor::Editing` and
//! `koru_workspace_host::View` over the document app's `BuiltInDocument`,
//! which embeds `koru_editor::Editor`. It paints nothing of its own: every
//! draw call is delegated, so its record is support, not a painter.

use super::*;

const LEAF_FILE: &str = "apps/koru/src/leaf.rs";

/// The leaf's record.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: Leaf::document opens a BuiltInDocument over an
    // in-memory WorkspaceDoc and a node; no store, window or worker
    record(
        "H-APP-LEAF",
        "Leaf",
        Key::UiLabAppLeafName,
        Key::UiLabAppLeafPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(KORU, "koru::leaf", LEAF_FILE),
        DESKTOP_SHARED,
        &[
            host_only("koru::leaf::Leaf"),
            support("koru::leaf::Leaf::document"),
            support("koru::leaf::Leaf::set_read_only"),
            support("koru::leaf::Leaf::hide_title_row"),
            support("koru::leaf::Leaf::set_body_placeholder"),
            support("koru::leaf::Leaf::set_title_display"),
            support("koru::leaf::Leaf::title_ai_action_rect"),
            support("koru::leaf::Leaf::search_panel"),
            painter("koru::leaf::Leaf::draw_menu"),
            painter("koru::leaf::Leaf::draw_lifted_viewer"),
            support("koru::leaf::Leaf::caret_spot"),
            support("koru::leaf::Leaf::catch_up_at_viewport"),
            support("koru::leaf::Leaf::native_ime_target"),
            support("koru::leaf::Leaf::native_ime"),
            support("koru::leaf::Leaf::native_ime_cancel"),
            support("koru::leaf::Leaf::take_reminder_request"),
            support("koru::leaf::Leaf::take_entity_search_request"),
            support("koru::leaf::Leaf::take_delegate_request"),
        ],
    ),
];
