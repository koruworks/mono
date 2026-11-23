// SPDX-License-Identifier: MPL-2.0

//! **The Tasks page** (`apps/koru/src/tasks_view`): one bucket or list of
//! the workspace's tasks as a `koru_ui::table` of rows, with its heading,
//! filter chips, overflow menu, capture row and the editor's attribute
//! chips and pickers on each row. Inside a mini app the same view runs in
//! collection-only mode, which is the fixed catalog's `TaskCollection`.

use super::*;

const VIEW: &str = "apps/koru/src/tasks_view/view.rs";

/// The Tasks page's record.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: new(target) is state only; rows arrive from the store's
    // tasks query, which the container runs and feeds back (query.rs), so
    // a story needs a scripted answer rather than a store
    record(
        "C-TASKS-VIEW",
        "TasksView",
        Key::UiLabTasksPageName,
        Key::UiLabTasksPagePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::tasks_view", VIEW),
        DESKTOP_SHARED,
        &[
            host_only("koru::tasks_view::TasksView"),
            component(
                "koru::tasks_view::TasksView::collection_only",
                "TaskCollection",
            ),
            support("koru::tasks_view::TasksView::new"),
            support("koru::tasks_view::TasksView::target"),
            support("koru::tasks_view::TasksView::set_target"),
            support("koru::tasks_view::ListFace"),
            support("koru::tasks_view::LaneItem"),
            support("koru::tasks_view::Request"),
            support("koru::tasks_view::TaskEntry"),
            support("koru::tasks_view::due_sentence"),
            support("koru::tasks_view::row_menu"),
            support("koru::tasks_view::rows_menu"),
            painter("koru::tasks_view::heading::tree"),
            painter("koru::tasks_view::capture::Capture"),
            painter("koru::tasks_view::capture::paint_empty"),
            painter("koru::tasks_view::draw::draw_swatch"),
            support("koru::tasks_view::states::empty_state"),
            support("koru::tasks_view::menu::list_picker_items"),
            support("koru::tasks_view::menu::overflow_items"),
            support("koru::tasks_view::menu::color_picker_items"),
            support("koru::tasks_view::accessibility::Context"),
        ],
    ),
];
