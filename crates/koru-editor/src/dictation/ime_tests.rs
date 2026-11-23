// SPDX-License-Identifier: MPL-2.0

use crate::{Editing, Editor};
use koru_core::{
    Mark, NodeId, WorkspaceId,
    node::Node,
    schema::{Parent, WorkspaceDoc},
};
use winit::event::Ime;

fn fixture(text: &str) -> (WorkspaceDoc, NodeId, Editor) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "ime fixture").unwrap();
    let id = ws
        .insert(Parent::Root, Node::paragraph(text).unwrap())
        .unwrap();
    ws.enable_undo();
    let mut editor = Editor::over(&ws);
    editor.view.active = true;
    (ws, id, editor)
}

fn text(ws: &WorkspaceDoc, id: NodeId) -> String {
    ws.read(id).unwrap().content.as_text().unwrap().to_owned()
}

#[test]
fn native_preedit_cursor_and_rebuild_do_not_author_text() {
    let (mut ws, id, mut editor) = fixture("ab");
    editor.body.rows[0].select_range_now(1, 1);
    let before = ws.version();
    assert!(editor.ime(&mut ws, &Ime::Preedit("日本".into(), Some((3, 3)))));
    assert_eq!(editor.body.rows[0].text, "a日本b");
    assert_eq!(editor.body.rows[0].caret(), 4);
    let reflows = editor.view.reflows;
    editor.ime(&mut ws, &Ime::Preedit("日本".into(), Some((0, 3))));
    assert_eq!(
        editor.view.reflows, reflows,
        "cursor-only event does not reflow"
    );
    assert_eq!(editor.body.rows[0].directed_range(), Some((1, 4)));
    editor.body.rebuild(&ws);
    editor.reflow();
    assert_eq!(editor.body.rows[0].directed_range(), Some((1, 4)));
    editor.ime(&mut ws, &Ime::Preedit("日本".into(), None));
    assert!(editor.ime_cursor_area().is_none());
    editor.body.rebuild(&ws);
    editor.reflow();
    assert!(editor.ime_cursor_area().is_none());
    assert_eq!(ws.version(), before);
    assert_eq!(text(&ws, id), "ab");
    assert!(!ws.undo().unwrap_or(false));
}

#[test]
fn native_empty_preedit_keeps_replacement_and_commit_is_one_undo() {
    let (mut ws, id, mut editor) = fixture("one old end");
    editor.body.rows[0].select_range_now(7, 4);
    editor.ime(&mut ws, &Ime::Preedit("新".into(), Some((3, 3))));
    editor.ime(&mut ws, &Ime::Preedit(String::new(), None));
    assert_eq!(editor.body.rows[0].text, "one old end");
    assert!(editor.body.composition().is_some());
    editor.ime(&mut ws, &Ime::Commit("新しい".into()));
    assert_eq!(text(&ws, id), "one 新しい end");
    assert!(ws.undo().unwrap());
    assert_eq!(text(&ws, id), "one old end");
    assert!(!ws.undo().unwrap_or(false));
}

#[test]
fn native_cancel_restores_direction_and_armed_formatting() {
    let (mut ws, id, mut editor) = fixture("one old end");
    editor.body.rows[0].select_range_now(7, 4);
    let before = ws.version();
    editor.ime(&mut ws, &Ime::Preedit("日本".into(), Some((6, 6))));
    editor.ime_cancel(&mut ws);
    assert_eq!(editor.body.rows[0].directed_range(), Some((7, 4)));
    assert_eq!(text(&ws, id), "one old end");
    assert_eq!(ws.version(), before);
    assert!(!ws.undo().unwrap_or(false));

    editor.body.rows[0].select_range_now(11, 11);
    editor.body.rows[0].toggle_mark(&mut ws, Mark::Bold);
    let armed = editor.body.rows[0].armed();
    editor.ime(&mut ws, &Ime::Preedit("仮".into(), Some((3, 3))));
    editor.body.rebuild(&ws);
    editor.reflow();
    editor.ime_cancel(&mut ws);
    assert_eq!(editor.body.rows[0].armed(), armed);
    editor.ime(&mut ws, &Ime::Commit("本".into()));
    assert!(ws.marks_in(id, 11..14).unwrap().has(Mark::Bold));
    assert!(ws.undo().unwrap());
    assert_eq!(text(&ws, id), "one old end");
    assert!(!ws.undo().unwrap_or(false));
}

#[test]
fn native_direct_commit_and_empty_commit_follow_typing_selection() {
    let (mut ws, id, mut editor) = fixture("keep");
    editor.body.rows[0].select_range_now(4, 0);
    editor.ime(&mut ws, &Ime::Commit(String::new()));
    assert_eq!(text(&ws, id), "keep");
    assert_eq!(editor.body.rows[0].directed_range(), Some((4, 0)));
    editor.ime(&mut ws, &Ime::Commit("保つ".into()));
    assert_eq!(text(&ws, id), "保つ");
}

#[test]
fn native_and_voice_cannot_commit_or_cancel_each_other() {
    let (mut ws, id, mut editor) = fixture("start");
    editor.body.rows[0].select_range_now(5, 5);
    editor.ime(&mut ws, &Ime::Preedit("仮".into(), Some((3, 3))));
    assert!(!editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "voice");
    editor.commit_dictation(&mut ws, "voice");
    editor.cancel_dictation(&mut ws);
    assert_eq!(editor.body.rows[0].text, "start仮");
    editor.ime_cancel(&mut ws);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "voice");
    assert!(!editor.ime(&mut ws, &Ime::Commit("native".into())));
    assert!(!editor.ime_cancel(&mut ws));
    assert_eq!(text(&ws, id), "start");
    editor.commit_dictation(&mut ws, "voice");
    assert_eq!(text(&ws, id), "startvoice");
}

#[test]
fn native_invalid_byte_cursor_hides_caret_without_panicking() {
    let (mut ws, _, mut editor) = fixture("");
    editor.ime(&mut ws, &Ime::Preedit("日本".into(), Some((1, usize::MAX))));
    assert!(editor.ime_cursor_area().is_none());
    assert_eq!(editor.body.rows[0].text, "日本");
}

#[test]
fn native_wrapped_cursor_scrolls_into_view_without_cursor_only_reflow() {
    let (mut ws, _, mut editor) = fixture("");
    editor.set_viewport(240.0, 100.0);
    let words = "日本語 ".repeat(30);
    editor.ime(
        &mut ws,
        &Ime::Preedit(words.clone(), Some((words.len(), words.len()))),
    );
    let scrolled = editor.view.scroll;
    assert!(
        scrolled > 0.0,
        "preedit follows its cursor below the viewport"
    );
    let reflows = editor.view.reflows;
    editor.ime(&mut ws, &Ime::Preedit(words.clone(), Some((0, 0))));
    assert!(editor.view.scroll < scrolled);
    assert_eq!(editor.view.reflows, reflows);
    let before_hidden = editor.view.scroll;
    editor.ime(&mut ws, &Ime::Preedit(words, None));
    assert_eq!(editor.view.scroll, before_hidden);
}

#[test]
fn native_commit_runs_task_and_word_boundary_hooks_once() {
    let (mut ws, id, mut editor) = fixture("task");
    koru_core::tasks::ensure_task(&mut ws, id, koru_core::attributes::TaskReason::Explicit)
        .unwrap();
    editor.body.rebuild(&ws);
    editor.reflow();
    editor.body.rows[0].select_range_now(4, 4);
    editor.ime(&mut ws, &Ime::Preedit(" 仮".into(), Some((4, 4))));
    assert!(editor.body.take_task_text_edits().is_empty());
    editor.ime(&mut ws, &Ime::Commit(" 日本語 ".into()));
    let edits = editor.body.take_task_text_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].node_id(), id);
    assert_eq!(editor.view.word_completed, Some(id));
    assert!(editor.body.take_task_text_edits().is_empty());
}

#[test]
fn native_peer_adoption_resolves_selection_and_deleted_target_never_retargets_commit() {
    let (mut ws, id, mut editor) = fixture("one old end");
    let sibling = ws
        .insert(Parent::Root, Node::paragraph("neighbor").unwrap())
        .unwrap();
    editor.body.rebuild(&ws);
    editor.body.focus_on(id);
    editor.reflow();
    editor.body.rows[0].select_range_now(7, 4);
    editor.ime(&mut ws, &Ime::Preedit("新".into(), Some((3, 3))));
    let mut peer = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
    peer.insert_text(id, 0, "prefix ").unwrap();
    ws.import(&peer.export_all_updates().unwrap()).unwrap();
    editor.body.defer_pending(&ws);
    editor.body.catch_up(&ws);
    editor.reflow();
    editor.ime_cancel(&mut ws);
    assert_eq!(
        editor.body.row(id).unwrap().directed_range(),
        Some((14, 11))
    );
    editor.ime(&mut ws, &Ime::Preedit("新".into(), Some((3, 3))));
    peer.delete(id).unwrap();
    ws.import(&peer.export_all_updates().unwrap()).unwrap();
    editor.body.defer_pending(&ws);
    editor.body.catch_up(&ws);
    editor.reflow();
    let before = ws.version();
    editor.ime(&mut ws, &Ime::Commit("must not land".into()));
    assert_eq!(ws.version(), before);
    assert_eq!(text(&ws, sibling), "neighbor");
}
