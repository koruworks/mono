// SPDX-License-Identifier: MPL-2.0

//! Tests for the editor.
//!
//! A child module rather than an inline `mod tests`, so the file a reader opens
//! is the code. Still the same crate, so private items stay reachable through
//! `use super::*` — which is why this is not an integration test under `tests/`.

use crate::leaf::Leaf;
use chrono::NaiveDate;
use koru_core::node::Align;
use koru_core::node::Heading;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Attr, AttrValue, DocumentId, Mark, NodeId, ProfileId, Ref, WorkspaceId};
use koru_draw::pill::PillKind;
use koru_draw::theme;
use koru_editor::components::{Overlay, link_prompt};
use koru_editor::trigger::is_trigger_position;
use koru_editor::{Editing, Editor, Hover, rowlayout};
use koru_workspace::{self, Workspace};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

#[test]
fn trigger_after_a_pill_anchor_is_not_a_word_boundary() {
    let mut e = one_row("x");
    assert!(!is_trigger_position(e.body().focused().unwrap(), 1));
    e.key(&named(NamedKey::End), None, NONE);
    e.key(&ch(" "), Some(" "), NONE);
    assert!(is_trigger_position(e.body().focused().unwrap(), 2));
}

// ------------------------------------------------------------ key map
//
// These exist because `Editor::key` used to take a whole `winit::KeyEvent`,
// which cannot be constructed in a test — so the key map was unverifiable,
// and Ctrl+K and Alt+Arrow both shipped broken.

fn named(n: NamedKey) -> Key {
    Key::Named(n)
}

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

use koru_keys::{NONE, PRIMARY, SHIFT};

/// The theme is a process-wide atomic (see `theme::p`), so any test that
/// flips it has to be serialised against the others — cargo runs tests
/// concurrently in one process. Poisoning is ignored: a panic in one test
/// should not cascade into unrelated failures.
#[allow(dead_code)]
static THEME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[allow(dead_code)]
pub(crate) fn lock_theme() -> std::sync::MutexGuard<'static, ()> {
    THEME_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Compatibility wrapper for tests written while the window menu was
/// experimental. The overflow button is now always present.
pub(crate) fn with_menu<T>(_on: bool, f: impl FnOnce() -> T) -> T {
    f()
}

/// An editor and the document it edits, in one value.
///
/// The editor borrows the document per call now, which is what lets two
/// views share one — but a test wants to hold both. Deref means every test
/// body still says `e.body()`, `e.view().overlay`; the shadowing methods below
/// feed the document in where it is needed.
struct Harness {
    ws: WorkspaceDoc,
    /// A `Leaf`, not a bare `Editor`, so a test drives the same dispatch the
    /// shell does. Tab means different things in the two views, and a harness
    /// that skipped that would pass while the app was broken.
    ///
    /// And a `Leaf` rather than the container above it, deliberately: a leaf on
    /// its own still believes it owns the window, so every coordinate in this
    /// file stays window-origin. The container insets its view by a sidebar and
    /// a caption row, and testing through it would have shifted every click in
    /// two thousand lines to prove nothing. What the container adds — that the
    /// offset arrives correctly — is worth one test of its own, and has one.
    editor: Leaf,
}

impl std::ops::Deref for Harness {
    type Target = Leaf;
    fn deref(&self) -> &Leaf {
        &self.editor
    }
}

impl std::ops::DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Leaf {
        &mut self.editor
    }
}

impl Harness {
    fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        self.editor.key(&mut self.ws, key, text, mods)
    }

    fn mouse_down(&mut self, x: f32, y: f32, ctrl: bool) -> bool {
        self.editor.mouse_down(&mut self.ws, x, y, ctrl)
    }

    fn mouse_up(&mut self) -> bool {
        self.editor.mouse_up(&mut self.ws)
    }

    fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        self.editor.mouse_double_click(&self.ws, x, y)
    }

    fn complete_new_page_link(
        &mut self,
        title: String,
        target: koru_editor::trigger::DocumentTarget,
    ) -> bool {
        self.editor
            .complete_new_page_link(&mut self.ws, title, target)
    }

    /// Swap this harness onto the document view, showing `doc`.
    fn open_document(&mut self, doc: NodeId) {
        self.editor = Leaf::document(&self.ws, doc);
        self.editor.reflow();
    }
}

/// A non-flat, title-less document view rooted at `node`.
///
/// **Was `Leaf::list_block`, which only this file and `perf.rs` ever called.**
/// A constructor on `Leaf` that no production path reaches is a public API
/// maintained for a harness, and the dead-code warning it drew was read as an
/// invitation to delete it — which is a fair reading of what the warning says
/// and the wrong conclusion, because the *tests* are its users. Calling
/// `BuiltInDocument::open_list` here puts it where its callers are and leaves
/// `Leaf` describing only what the container builds.
///
/// The behaviour is unchanged and is what these tests depend on: `flat: false`
/// with **no title row**, so row 0 is the first real row rather than the
/// document's name.
fn list_block(ws: &WorkspaceDoc, node: NodeId) -> Leaf {
    Leaf::Document(koru_document::BuiltInDocument::open_list(ws, node))
}

/// Rows at the given depths, and nothing else. The shared shape behind every
/// structure test, so each one can state what it needs rather than inheriting it.
///
/// **The rows hang under a checkbox list block, and the view is rooted at it.**
///
/// They used to be the top of the document, shown by the task outline — a view
/// with no title row, so `rows[0]` was the first row a test wrote. The outline
/// is gone ([ADR-90](../../../docs/decisions/README.md)), and a document puts its own
/// name in row 0, which would shift every index in this file by one and change
/// what `is_title_row` guards under half of them.
///
/// A `Kind::ListBlock` root is what makes the substitution *behavioural* rather
/// than cosmetic: `list_block` below is the same editor with `flat: false` and
/// **no title row**, and a row under a list block carries a `list_style`, which
/// is the predicate the document view uses to decide that Tab nests rather than
/// typing a tab. So the rows here indent, reorder and tick exactly as the
/// outline's did — which is what these two thousand lines are about — and the
/// coordinates they were written in survive intact.
fn rows_at(rows: &[(&str, u16)]) -> Harness {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let root = ws
        .insert(Parent::Root, Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(root, Some(koru_core::ListStyle::Checkbox))
        .unwrap();
    let mut stack: Vec<NodeId> = Vec::new();
    for (text, depth) in rows {
        let parent = match depth {
            0 => Parent::Node(root),
            d => Parent::Node(stack[*d as usize - 1]),
        };
        let id = ws.insert(parent, Node::task(*text).unwrap()).unwrap();
        stack.truncate(*depth as usize);
        stack.push(id);
    }
    ws.enable_undo();
    let editor = list_block(&ws, root);
    Harness { ws, editor }
}

/// A parent with six children and a trailing empty line — the shape the
/// reorder, indent, zoom and crumb tests index into. Row 1 is already done,
/// because several need a row that is.
///
/// Deliberately plain: no pills, no mixed scripts, no per-row fonts. A test that
/// needs one of those adds it, so its premise is visible where it is used.
fn outline() -> Harness {
    let mut h = rows_at(&[
        ("Launch referral program", 0),
        ("Write spec", 1),
        ("Reward payout flow is blocked on billing", 1),
        ("Review the terms", 1),
        ("Check the numbers", 1),
        ("Ship it", 1),
        ("", 1),
    ]);
    let done = h.body().rows[1].id;
    h.ws.set_done(done, true).unwrap();
    h.editor.body_mut().rebuild(&h.ws);
    h.editor.reflow();
    h
}

/// Put the harness's editor into the state `BuiltInDocument::open` produces.
///
/// The harness holds a bare `Editor`, because almost every test is about shared
/// editing. The four document-only tests need its furniture, and this is the one
/// place that knows what that furniture is.
fn as_document(h: &mut Harness, doc: NodeId) {
    h.open_document(doc);
}

/// Append a pill to a row, the way typing one does — a space, then the anchor.
/// The index of the one row carrying a pill — whichever row that turned out
/// to be, so a test asserts on the pill rather than on the harness's shape.
fn pill_row(h: &Harness) -> usize {
    h.body()
        .rows
        .iter()
        .position(|row| !row.pills.is_empty())
        .expect("a row with a pill")
}

fn add_pill(h: &mut Harness, row: usize, attr: &Attr) -> koru_core::AttrId {
    let id = h.body().rows[row].id;
    let at = h.ws.text_len(id).unwrap();
    h.ws.insert_text(id, at, " ").unwrap();
    let anchor = h.ws.insert_pill(id, at + 1, attr).unwrap();
    h.editor.body_mut().refresh(&h.ws, id);
    h.editor.reflow();
    anchor
}

/// An editor holding exactly one row, with the caret at the start.
fn one_row(text: &str) -> Harness {
    rows_at(&[(text, 0)])
}

/// One **prose** row in a document, with the caret on it.
///
/// [`one_row`] gives a task inside a list, which is what almost everything here
/// wants — but a *block* slash-command (`/document`, `/image`, a heading) is
/// deliberately hidden inside a list item, so a test of one has to start
/// somewhere a block belongs. `rows[0]` is the document's own title and
/// `rows[1]` is the line, which is where `Leaf::document` puts the caret.
fn one_prose_row(text: &str) -> Harness {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("Doc").unwrap())
        .unwrap();
    ws.insert(Parent::Node(doc), Node::paragraph(text).unwrap())
        .unwrap();
    ws.enable_undo();
    let mut editor = Leaf::document(&ws, doc);
    editor.reflow();
    Harness { ws, editor }
}

/// Turn the heading level off, the way a view that offers no levels has it.
///
/// `headings_enabled` is a `ViewState` flag rather than a property of a kind of
/// view, and it was the task outline that shipped with it off. The outline is
/// gone; the flag and everything it gates are not, so the two tests that pin
/// what it does set it here rather than inheriting it from a harness.
fn without_headings(h: &mut Harness) {
    h.editor.view_mut().headings_enabled = false;
}

/// One row with `sel` selected.
fn with_selection(text: &str, sel: std::ops::Range<usize>) -> Harness {
    let mut e = one_row(text);
    e.editor.place_selection_pair((sel.start, sel.end));
    e.reflow();
    e
}

fn text_of(e: &Harness) -> String {
    e.body()
        .focused()
        .map(|r| r.text.clone())
        .unwrap_or_default()
}

/// Which byte ranges carry `mark`, read back out of the document.
fn marked(e: &Harness, mark: Mark) -> String {
    e.body()
        .focused()
        .map(|r| {
            r.runs
                .iter()
                .filter(|run| run.has(mark))
                .map(|run| run.text.as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn ctrl_k_opens_the_link_prompt_in_either_encoding() {
    for key in [ch("k"), ch("\u{0b}")] {
        let mut e = outline();
        assert!(!e.view().overlay.is_open());
        assert!(e.key(&key, None, PRIMARY));
        assert!(
            matches!(e.view().overlay, Overlay::Link(_)),
            "Ctrl+K should open the link prompt for {key:?}"
        );
    }
}

#[test]
fn ctrl_shift_arrows_reorder_the_focused_row() {
    let mut e = outline();
    // A row with siblings either side, so it can move both ways.
    e.body_mut().focus = 2;
    let moved = e.body().focused_id().unwrap();
    assert!(e.key(
        &named(NamedKey::ArrowUp),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT)
    ));
    assert_eq!(e.body().rows[1].id, moved, "row moved up");
    assert_eq!(e.body().focus, 1, "focus followed it");

    assert!(e.key(
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT)
    ));
    assert_eq!(e.body().rows[2].id, moved, "and back down");
    assert_eq!(e.body().focus, 2);
}

/// Windows routes Alt+key through the system menu, so the same action is
/// reachable with Ctrl+Shift+Arrow.
#[test]
fn ctrl_shift_arrows_also_reorder() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let moved = e.body().focused_id().unwrap();
    let mods = koru_keys::PRIMARY | ModifiersState::SHIFT;
    assert!(e.key(&named(NamedKey::ArrowUp), None, mods));
    assert_eq!(e.body().rows[1].id, moved);
}

/// A plain arrow must still move the caret, not the row.
#[test]
fn plain_arrows_do_not_reorder() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let before: Vec<_> = e.body().rows.iter().map(|r| r.id).collect();
    e.key(&named(NamedKey::ArrowUp), None, NONE);
    let after: Vec<_> = e.body().rows.iter().map(|r| r.id).collect();
    assert_eq!(before, after, "caret motion must not reorder rows");
}

#[test]
fn f4_hides_completed() {
    let mut e = outline();
    assert!(!e.body().hide_completed);
    let before = e.body().rows.len();
    assert!(e.key(&named(NamedKey::F4), None, NONE));
    assert!(e.body().hide_completed, "F4 hides completed rows");
    assert!(e.body().rows.len() < before, "and a row actually went away");
}

#[test]
fn typing_inserts_and_slash_arms_the_command_menu() {
    let mut e = outline();
    assert!(e.key(&ch("/"), Some("/"), NONE));
    assert!(
        matches!(e.view().overlay, Overlay::Command(_)),
        "a lone / at a word boundary opens the menu"
    );
}

#[test]
fn escape_closes_an_open_overlay() {
    let mut e = outline();
    e.key(&ch("k"), None, PRIMARY);
    assert!(e.view().overlay.is_open());
    e.key(&named(NamedKey::Escape), None, NONE);
    assert!(!e.view().overlay.is_open(), "Escape dismisses the prompt");
}

#[test]
fn tab_and_shift_tab_indent_and_outdent() {
    let mut e = outline();
    e.body_mut().focus = 2; // a depth-1 row under a depth-0 parent
    let start = e.body().focused().unwrap().depth;
    assert!(e.key(&named(NamedKey::Tab), None, SHIFT));
    assert_eq!(
        e.body().focused().unwrap().depth,
        start - 1,
        "Shift+Tab outdents"
    );
    assert!(e.key(&named(NamedKey::Tab), None, NONE));
    assert_eq!(e.body().focused().unwrap().depth, start, "Tab indents");
}

/// The arrows no longer touch structure at all — they are the caret's.
#[test]
fn no_arrow_chord_indents_or_reorders() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let depth = e.body().focused().unwrap().depth;
    let id = e.body().focused_id().unwrap();

    for key in [NamedKey::ArrowLeft, NamedKey::ArrowRight] {
        for mods in [ModifiersState::ALT, PRIMARY.union(SHIFT)] {
            e.key(&named(key), None, mods);
        }
    }
    assert_eq!(e.body().focused().unwrap().depth, depth, "depth untouched");

    for mods in [ModifiersState::ALT] {
        e.key(&named(NamedKey::ArrowUp), None, mods);
    }
    assert_eq!(e.body().rows[2].id, id, "Alt+Up no longer reorders");
}

/// The rule from the brief: a child of a level-1 task can reach level 2,
/// never level 3, no matter how many times you press. With a real tree this
/// is structural rather than a clamp — there is simply no sibling above to
/// move under a second time.
#[test]
fn indent_can_never_skip_a_level() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let above = e.body().rows[1].depth;
    for _ in 0..6 {
        e.key(&named(NamedKey::Tab), None, NONE);
    }
    assert_eq!(
        e.body().focused().unwrap().depth,
        above + 1,
        "stuck at one deeper than the row above"
    );
}

#[test]
fn plain_left_right_move_the_caret_not_the_depth() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let before = e.body().focused().unwrap().depth;
    e.key(&named(NamedKey::ArrowRight), None, NONE);
    e.key(&named(NamedKey::ArrowLeft), None, NONE);
    assert_eq!(e.body().focused().unwrap().depth, before);
}

// ------------------------------------------------------------ formatting

/// The delimiters are gone: Ctrl+B marks the range and leaves the text
/// alone, which is the whole point of moving formatting into the CRDT.
#[test]
fn ctrl_b_i_u_mark_the_selection_without_touching_the_text() {
    for (letter, mark) in [
        ('b', Mark::Bold),
        ('i', Mark::Italic),
        ('u', Mark::Underline),
    ] {
        let mut e = with_selection("a bcd e", 2..5);
        assert!(e.key(&ch(&letter.to_string()), None, PRIMARY));
        assert_eq!(text_of(&e), "a bcd e", "Ctrl+{letter} inserts nothing");
        assert_eq!(marked(&e, mark), "bcd");
    }
}

#[test]
fn the_shortcuts_toggle_off_again() {
    let mut e = with_selection("a bcd e", 2..5);
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "bcd");
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "", "second press clears it");
}

/// The selection has to survive, so a second chord applies to the same text.
#[test]
fn the_selection_still_covers_the_same_text_after_marking() {
    let mut e = with_selection("a bcd e", 2..5);
    e.key(&ch("b"), None, PRIMARY);
    let sel = e.body().focused().unwrap().selection.text_range();
    assert_eq!(&text_of(&e)[sel], "bcd", "selection followed the text");

    // ...which is what lets bold and italic stack.
    e.key(&ch("i"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "bcd");
    assert_eq!(marked(&e, Mark::Italic), "bcd");
    assert_eq!(text_of(&e), "a bcd e");
}

/// A partly-bold selection becomes all bold — intersection, not union.
#[test]
fn marking_a_partly_marked_selection_marks_all_of_it() {
    let mut e = with_selection("abcdef", 0..3);
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "abc");

    e.editor.place_selection_pair((0, 6));
    e.reflow();
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "abcdef", "the rest joined it");
}

/// With no selection the chord arms formatting for the next keystroke.
/// There is nothing to put in the document until there is text to mark.
#[test]
fn a_collapsed_caret_arms_the_next_keystroke() {
    let mut e = with_selection("ab", 2..2);
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(text_of(&e), "ab", "nothing inserted yet");

    e.key(&ch("x"), Some("x"), NONE);
    assert_eq!(text_of(&e), "abx");
    assert_eq!(marked(&e, Mark::Bold), "x", "only the new letter is bold");
}

/// Marks expand rightwards, so typing after a bold word inherits it — and
/// Ctrl+B has to be able to turn that back off.
#[test]
fn arming_can_turn_an_inherited_mark_off() {
    let mut e = with_selection("ab", 0..2);
    e.key(&ch("b"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "ab");

    e.editor.place_selection_pair((2, 2));
    e.reflow();
    e.key(&ch("b"), None, PRIMARY); // disarm
    e.key(&ch("c"), Some("c"), NONE);
    assert_eq!(text_of(&e), "abc");
    assert_eq!(marked(&e, Mark::Bold), "ab", "the new letter stayed plain");
}

#[test]
fn ctrl_backslash_clears_every_mark_on_the_selection() {
    let mut e = with_selection("a bcd e", 2..5);
    e.key(&ch("b"), None, PRIMARY);
    e.key(&ch("i"), None, PRIMARY);
    assert_eq!(marked(&e, Mark::Bold), "bcd");

    assert!(e.key(&ch("\\"), None, PRIMARY));
    assert_eq!(text_of(&e), "a bcd e");
    assert_eq!(marked(&e, Mark::Bold), "");
    assert_eq!(marked(&e, Mark::Italic), "");
    let sel = e.body().focused().unwrap().selection.text_range();
    assert_eq!(&text_of(&e)[sel], "bcd", "selection survives the strip");
}

/// Windows delivers Ctrl+`\` as the ASCII file separator, 0x1C, exactly as it
/// delivers Ctrl+K as 0x0B. Both encodings have to reach the same chord.
#[test]
fn ctrl_backslash_clears_in_either_encoding() {
    for key in [ch("\\"), ch("\u{1c}")] {
        let mut e = with_selection("a bcd e", 2..5);
        e.key(&ch("b"), None, PRIMARY);
        assert_eq!(marked(&e, Mark::Bold), "bcd");
        assert!(e.key(&key, None, PRIMARY), "{key:?} should clear");
        assert_eq!(marked(&e, Mark::Bold), "", "{key:?}");
    }
}

#[test]
fn ctrl_backslash_on_unformatted_text_is_a_no_op() {
    let mut e = with_selection("plain text", 0..5);
    e.key(&ch("\\"), None, PRIMARY);
    assert_eq!(text_of(&e), "plain text");
    assert_eq!(marked(&e, Mark::Bold), "");
}

/// Space is a character key, and a chord on it competes with typing one. It is
/// bound to nothing now — and must stay bound to nothing, rather than quietly
/// acquiring a meaning that fires while somebody is typing.
#[test]
fn ctrl_space_is_bound_to_nothing() {
    assert_eq!(
        koru_editor::input::keymap::resolve(&named(NamedKey::Space), Some(" "), PRIMARY),
        None
    );
    let mut e = with_selection("a bcd e", 2..5);
    e.key(&ch("b"), None, PRIMARY);
    assert!(
        !e.key(&named(NamedKey::Space), None, PRIMARY),
        "nothing to do"
    );
    assert_eq!(marked(&e, Mark::Bold), "bcd", "and nothing was cleared");
}

/// Windows delivers Ctrl+I as Tab. It must italicise, not indent.
#[test]
fn ctrl_i_arriving_as_tab_does_not_indent() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let depth = e.body().focused().unwrap().depth;
    let end = e.body().focused().unwrap().text.len();
    e.editor.place_selection_pair((0, end));
    e.reflow();

    assert!(e.key(&named(NamedKey::Tab), None, PRIMARY));
    assert!(
        !marked(&e, Mark::Italic).is_empty(),
        "Ctrl+Tab means italic"
    );
    assert_eq!(
        e.body().focused().unwrap().depth,
        depth,
        "and never indents"
    );
}

/// ...while a plain Tab still indents.
#[test]
fn plain_tab_still_indents() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let before = e.body().focused().unwrap().depth;
    e.key(&named(NamedKey::Tab), None, SHIFT);
    assert_eq!(e.body().focused().unwrap().depth, before - 1);
    assert!(e.key(&named(NamedKey::Tab), None, NONE));
    assert_eq!(
        e.body().focused().unwrap().depth,
        before,
        "Tab indents again"
    );
}

/// Marking a run that contains a pill must not disturb its anchor.
#[test]
fn marking_text_around_a_pill_keeps_the_pill() {
    let mut e = outline();
    add_pill(
        &mut e,
        2,
        &Attr::labelled(
            "link",
            AttrValue::Url("https://koru.example.com/terms".into()),
            "Terms page",
        ),
    );
    e.body_mut().focus = 2;
    let before = e.body().focused().unwrap().pills.len();
    assert_eq!(before, 1);
    let end = e.body().focused().unwrap().text.len();
    e.editor.place_selection_pair((0, end));
    e.reflow();

    e.key(&ch("b"), None, PRIMARY);
    let row = e.body().focused().unwrap();
    assert_eq!(row.pills.len(), before, "the pill survived being marked");
    assert!(
        row.runs.iter().any(|r| r.is_pill()),
        "and its anchor is still in the text"
    );
}

/// Strikethrough is a fourth flag, so it rides the same path Ctrl+B does.
#[test]
fn ctrl_shift_s_strikes_the_selection() {
    let mut e = with_selection("a bcd e", 2..5);
    let mods = koru_keys::PRIMARY | ModifiersState::SHIFT;
    assert!(e.key(&ch("s"), None, mods));
    assert_eq!(marked(&e, Mark::Strike), "bcd");
    assert_eq!(text_of(&e), "a bcd e", "nothing inserted");

    assert!(e.key(&ch("s"), None, mods));
    assert_eq!(marked(&e, Mark::Strike), "", "and it toggles off");
}

/// A value mark, unlike a flag, cycles through its options — there is one
/// chord and four colours.
#[test]
fn ctrl_shift_h_cycles_the_highlight_colours() {
    let mut e = with_selection("a bcd e", 2..5);
    let mods = koru_keys::PRIMARY | ModifiersState::SHIFT;
    let names = theme::highlight_names();

    for expected in names {
        assert!(e.key(&ch("h"), None, mods));
        let lit: Vec<_> = e
            .body()
            .focused()
            .unwrap()
            .runs
            .iter()
            .filter(|r| r.style.highlight.as_deref() == Some(expected))
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(lit, vec!["bcd"], "expected {expected}");
    }

    // One more press takes it off again.
    assert!(e.key(&ch("h"), None, mods));
    assert!(
        e.body()
            .focused()
            .unwrap()
            .runs
            .iter()
            .all(|r| r.style.highlight.is_none()),
        "the cycle ends at no highlight"
    );
}

/// "Clear formatting" means all of it — a highlight left behind after
/// Ctrl+`\` is not what anybody means by the phrase.
#[test]
fn ctrl_backslash_clears_values_as_well_as_flags() {
    let mut e = with_selection("a bcd e", 2..5);
    let shift = koru_keys::PRIMARY | ModifiersState::SHIFT;
    e.key(&ch("b"), None, PRIMARY);
    e.key(&ch("h"), None, shift);
    assert_eq!(marked(&e, Mark::Bold), "bcd");

    assert!(e.key(&ch("\\"), None, PRIMARY));
    assert_eq!(marked(&e, Mark::Bold), "");
    assert!(
        e.body()
            .focused()
            .unwrap()
            .runs
            .iter()
            .all(|r| r.style.highlight.is_none()),
        "the highlight went with the bold"
    );
}

/// Alignment ignores the selection: it is a property of the block.
#[test]
fn ctrl_l_e_r_align_the_row() {
    let mut e = with_selection("a bcd e", 2..5);
    let mods = koru_keys::PRIMARY;

    assert!(e.key(&ch("e"), None, mods));
    assert_eq!(e.body().focused().unwrap().align, Align::Center);

    assert!(e.key(&ch("r"), None, mods));
    assert_eq!(e.body().focused().unwrap().align, Align::End);

    assert!(e.key(&ch("l"), None, mods));
    assert_eq!(e.body().focused().unwrap().align, Align::Start);

    assert_eq!(text_of(&e), "a bcd e", "and never touches the text");
}

/// Pressing the same alignment twice returns to the default, so one chord
/// is both apply and undo.
#[test]
fn pressing_an_alignment_twice_clears_it() {
    let mut e = one_row("row");
    let mods = koru_keys::PRIMARY;

    e.key(&ch("e"), None, mods);
    assert_eq!(
        e.ws.read(e.body().rows[0].id).unwrap().meta.align,
        Some(Align::Center)
    );

    e.key(&ch("e"), None, mods);
    assert_eq!(
        e.ws.read(e.body().rows[0].id).unwrap().meta.align,
        None,
        "back to the default, not stored as Start"
    );
}

// ------------------------------------------------------------- headings

/// An empty document with one body row, focused — the state every heading test
/// starts from.
fn a_document() -> Harness {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    let row = h.ws.insert(Parent::Node(doc), Node::paragraph("").unwrap());
    row.unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();
    h
}

/// The first row of a document's *body*.
///
/// Row 0 is the document's own name — the title is a row now, not a label in a
/// strip — so a test about the first paragraph has to say so. Named rather than
/// written as `1` because "why one?" is exactly the question a bare index
/// leaves unanswered.
const BODY: usize = 1;

use koru_keys::PRIMARY_ALT;

/// A heading is a property of the block, so it ignores the selection the way
/// alignment does — and it reaches the document as well as the row cache.
#[test]
fn ctrl_alt_1_to_4_set_the_heading_level() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    for (digit, level) in [
        ("1", Heading::H1),
        ("2", Heading::H2),
        ("3", Heading::H3),
        ("4", Heading::H4),
    ] {
        assert!(h.key(&ch(digit), None, PRIMARY_ALT), "Ctrl+Alt+{digit}");
        assert_eq!(h.body().rows[BODY].heading, Some(level));
        assert_eq!(h.ws.read(id).unwrap().meta.heading, Some(level));
    }
}

/// The chord has to reach the *layout*, not just the document — a heading that
/// records but does not redraw is the failure mode `adopt`'s comparison exists
/// to prevent, and it is invisible to every assertion about `meta`.
#[test]
fn setting_a_heading_makes_the_row_taller_on_the_next_frame() {
    let mut h = a_document();
    let before = h.body().rows[BODY].height;

    h.key(&ch("1"), None, PRIMARY_ALT);

    let after = h.body().rows[BODY].height;
    assert!(
        after > before + 8.0,
        "an H1 row should grow: {before} -> {after}"
    );
}

/// One control is both apply and undo, the bargain the alignment buttons make.
#[test]
fn pressing_the_same_heading_twice_returns_to_body_text() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    h.key(&ch("2"), None, PRIMARY_ALT);
    assert_eq!(h.ws.read(id).unwrap().meta.heading, Some(Heading::H2));

    h.key(&ch("2"), None, PRIMARY_ALT);
    assert_eq!(
        h.ws.read(id).unwrap().meta.heading,
        None,
        "removed, not stored as a level"
    );
}

/// Ctrl+Alt+0 is body text unconditionally, whatever the line was.
#[test]
fn ctrl_alt_0_clears_a_heading() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    h.key(&ch("3"), None, PRIMARY_ALT);
    h.key(&ch("0"), None, PRIMARY_ALT);
    assert_eq!(h.ws.read(id).unwrap().meta.heading, None);
}

/// A view that offers no heading levels ignores the chord. The key map is
/// shared, so `Ctrl+Alt+2` still *resolves* — what must not happen is the row
/// quietly becoming an H2 in a view with no way to show or undo it.
#[test]
fn a_view_without_levels_ignores_a_heading_chord() {
    let mut e = one_row("a task");
    without_headings(&mut e);
    let id = e.body().rows[0].id;

    e.key(&ch("2"), None, PRIMARY_ALT);
    assert_eq!(e.ws.read(id).unwrap().meta.heading, None);
    assert_eq!(e.body().rows[0].heading, None);
}

/// `## ` at the start of a line is the toolbar button, typed. The hashes are
/// taken back out — there are no markdown delimiters in the text.
#[test]
fn a_hash_and_a_space_at_the_start_of_a_line_makes_it_a_heading() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    for c in "## ".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }

    assert_eq!(h.ws.read(id).unwrap().meta.heading, Some(Heading::H2));
    assert_eq!(h.body().rows[BODY].text, "", "the hashes are gone");
    assert_eq!(h.body().rows[BODY].caret(), 0);
}

/// Only at the start, and only on a line that is not already a heading —
/// otherwise a sentence could not contain a `#` at all.
#[test]
fn a_hash_in_the_middle_of_a_line_is_just_a_hash() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    for c in "a # b".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }

    assert_eq!(h.ws.read(id).unwrap().meta.heading, None);
    assert_eq!(h.body().rows[BODY].text, "a # b");
}

/// The shorthand is one undo step: Ctrl+Z puts the hashes back *and* takes the
/// heading off, rather than leaving a heading with no way to see why.
#[test]
fn undoing_the_shorthand_puts_the_hashes_back_and_takes_the_heading_off() {
    let mut h = a_document();
    h.ws.enable_undo();
    let id = h.body().rows[BODY].id;

    for c in "# ".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    assert_eq!(h.ws.read(id).unwrap().meta.heading, Some(Heading::H1));

    h.key(&ch("z"), None, PRIMARY);
    assert_eq!(h.ws.read(id).unwrap().meta.heading, None);
    assert_eq!(
        h.ws.read(id).unwrap().content.as_text(),
        Some("#"),
        "the literal text is back"
    );
}

/// The same for the typed shorthand: where there are no levels, `#` is a `#`.
#[test]
fn the_shorthand_does_nothing_in_a_view_that_does_not_offer_headings() {
    let mut e = one_row("");
    without_headings(&mut e);
    let id = e.body().rows[0].id;

    for c in "# ".chars() {
        e.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }

    assert_eq!(e.ws.read(id).unwrap().meta.heading, None);
    assert_eq!(e.body().rows[0].text, "# ");
}

/// `/header2` names the level outright. The command text goes with it — what
/// the menu stood in for was typing, and nothing replaces it.
#[test]
fn choosing_a_heading_from_the_slash_menu_removes_the_command_text() {
    let mut h = a_document();
    let id = h.body().rows[BODY].id;

    for c in "/header2".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    assert!(matches!(h.view().overlay, Overlay::Command(_)));

    h.key(&named(NamedKey::Enter), None, NONE);
    assert_eq!(h.ws.read(id).unwrap().meta.heading, Some(Heading::H2));
    assert_eq!(
        h.body().rows[BODY].text,
        "",
        "the command text went with it"
    );
    assert!(matches!(h.view().overlay, Overlay::None));
}

#[test]
fn choosing_a_divider_from_the_slash_menu_creates_it_in_one_enter() {
    use koru_core::node::Kind;
    let mut h = a_document();

    for c in "/divider".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    assert!(matches!(h.view().overlay, Overlay::Command(_)));

    h.key(&named(NamedKey::Enter), None, NONE);
    assert!(
        h.body().rows.iter().any(|r| r.kind == Kind::Divider),
        "the divider is created on the first Enter"
    );
    assert!(
        !h.body().rows.iter().any(|r| r.text.contains("divider")),
        "no leftover command text"
    );
    assert!(matches!(h.view().overlay, Overlay::None));
    assert_eq!(
        h.body().focused().unwrap().kind,
        Kind::Paragraph,
        "the caret moved to the next paragraph, not the divider"
    );
}

// ------------------------------------------------------------ structure

#[test]
fn enter_splits_the_line_into_a_new_row() {
    let mut e = one_row("Reward payout");
    e.editor.place_caret(6);
    e.reflow();

    assert!(e.key(&named(NamedKey::Enter), None, NONE));
    assert_eq!(e.body().rows.len(), 2);
    assert_eq!(e.body().rows[0].text, "Reward");
    assert_eq!(e.body().rows[1].text, " payout");
    assert_eq!(e.body().focus, 1, "the caret follows the tail");
    assert_eq!(e.body().focused().unwrap().caret(), 0);
}

#[test]
fn backspace_at_the_start_merges_into_the_row_above() {
    // **Prose, not a list item.** `one_row` puts the caret in a checkbox list,
    // and Backspace at the start of a *list item* now un-lists it rather than
    // joining it to the item above — the behaviour every editor has, and the one
    // `backspace_unlists_the_first_document_paragraph_before_the_title_seam`
    // covers. This test is about the join, so it needs two rows that are not
    // list items: `one_prose_row` gives a title and a paragraph, and the join
    // happens between the paragraph and the one Enter makes below it.
    let mut e = one_prose_row("Reward");
    e.key(&named(NamedKey::End), None, NONE);
    e.key(&named(NamedKey::Enter), None, NONE);
    e.key(&ch("x"), Some("x"), NONE);
    assert_eq!(e.body().rows.len(), 3, "the title, the row and its new one");

    e.editor.place_caret(0);
    e.reflow();
    assert!(e.key(&named(NamedKey::Backspace), None, NONE));
    assert_eq!(e.body().rows.len(), 2);
    assert_eq!(text_of(&e), "Rewardx");
    assert_eq!(
        e.body().focused().unwrap().caret(),
        6,
        "the caret sits at the seam"
    );
}

/// `Ctrl+Backspace` takes the word behind the caret, not one character.
///
/// Both encodings: Windows delivers Backspace with `text` carrying 0x08, which
/// `ctrl_letter` reads as `h` — so this is also the guard against a letter
/// chord stealing the key on the way past.
#[test]
fn ctrl_backspace_deletes_a_word() {
    for text in [None, Some("\u{8}")] {
        let mut e = one_row("delete this word");
        e.key(&named(NamedKey::End), None, NONE);

        assert!(
            e.key(&named(NamedKey::Backspace), text, PRIMARY),
            "{text:?}"
        );

        assert_eq!(text_of(&e), "delete this ", "{text:?}");
    }
}

/// And plain Backspace still takes exactly one character.
#[test]
fn plain_backspace_deletes_one_character() {
    let mut e = one_row("word");
    e.key(&named(NamedKey::End), None, NONE);
    assert!(e.key(&named(NamedKey::Backspace), None, NONE));
    assert_eq!(text_of(&e), "wor");
}

/// One word at a time, all the way down — not two, and not the rest of the line.
#[test]
fn ctrl_backspace_takes_one_word_per_press() {
    let mut e = one_row("alpha beta gamma");
    e.key(&named(NamedKey::End), None, NONE);

    for expected in ["alpha beta ", "alpha ", ""] {
        e.key(&named(NamedKey::Backspace), None, PRIMARY);
        assert_eq!(text_of(&e), expected);
    }
}

/// **A pill is its own word.** The anchor is `U+2060 WORD JOINER`, whose whole
/// job in Unicode is to stop a break happening there — so parley read `task ⁠`
/// as one word and `Ctrl+Backspace` took the entire line in a single press.
#[test]
fn ctrl_backspace_over_a_pill_takes_only_the_pill() {
    let attr = Attr::new("due", AttrValue::Text("Friday".into()));
    let mut e = one_row("task");
    add_pill(&mut e, 0, &attr);
    e.key(&named(NamedKey::End), None, NONE);
    assert_eq!(text_of(&e), "task \u{2060}");

    e.key(&named(NamedKey::Backspace), None, PRIMARY);

    assert_eq!(text_of(&e), "task ", "the pill went, and nothing else");
    assert!(
        e.ws.read(e.body().rows[0].id)
            .unwrap()
            .content
            .attrs()
            .is_empty(),
        "and its attribute went with it"
    );
}

/// A word behind a pill stops at the pill rather than reaching across it.
#[test]
fn ctrl_backspace_does_not_reach_across_a_pill() {
    let attr = Attr::new("due", AttrValue::Text("Friday".into()));
    let mut e = one_row("task");
    add_pill(&mut e, 0, &attr);
    e.key(&named(NamedKey::End), None, NONE);
    for c in " tail".chars() {
        e.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }

    e.key(&named(NamedKey::Backspace), None, PRIMARY);

    assert_eq!(text_of(&e), "task \u{2060} ", "only `tail` went");
}

/// Undoing a word delete restores the word **and** the caret behind it. Left
/// where the shortened line ended, the caret sat in front of the text that had
/// just come back, and Ctrl+Backspace / Ctrl+Z was not a round trip.
#[test]
fn undoing_a_word_delete_puts_the_caret_back() {
    let mut e = one_row("alpha beta gamma");
    e.key(&named(NamedKey::End), None, NONE);
    let was = e.body().focused().unwrap().caret();

    e.key(&named(NamedKey::Backspace), None, PRIMARY);
    assert_eq!(text_of(&e), "alpha beta ");

    e.key(&ch("z"), None, PRIMARY);

    assert_eq!(text_of(&e), "alpha beta gamma");
    assert_eq!(e.body().focused().unwrap().caret(), was);
}

/// A run of word deletes is **one** undo step, so the caret comes back to
/// where it was before the *first* of them.
///
/// Recording the position on every delete meant the last one won, and undoing
/// three deletes at once put the caret two words short of where it started.
#[test]
fn undoing_a_run_of_word_deletes_puts_the_caret_back_to_the_start() {
    let mut e = one_row("alpha beta gamma delta");
    e.key(&named(NamedKey::End), None, NONE);
    let was = e.body().focused().unwrap().caret();

    for _ in 0..3 {
        e.key(&named(NamedKey::Backspace), None, PRIMARY);
    }
    assert_eq!(text_of(&e), "alpha ");

    e.key(&ch("z"), None, PRIMARY);

    assert_eq!(text_of(&e), "alpha beta gamma delta", "all three came back");
    assert_eq!(
        e.body().focused().unwrap().caret(),
        was,
        "and the caret with them"
    );
}

/// Deleting a *selection* comes back with the caret where the selection
/// **started**, not at the far end of it.
///
/// The caret sits at whichever end you stopped dragging at, so recording that
/// put it after text that had just come back — and dragging the same words
/// right-to-left gave a different answer from dragging them left-to-right.
#[test]
fn undoing_a_deleted_selection_puts_the_caret_at_its_start() {
    let mut e = with_selection("alpha beta gamma", 6..16);
    assert_eq!(&text_of(&e)[6..16], "beta gamma");

    e.key(&named(NamedKey::Backspace), None, PRIMARY);
    assert_eq!(text_of(&e), "alpha ");

    e.key(&ch("z"), None, PRIMARY);

    assert_eq!(text_of(&e), "alpha beta gamma");
    assert_eq!(e.body().focused().unwrap().caret(), 6);
}

/// The mirror: `Ctrl+Delete` forwards over a pill takes the pill and stops.
#[test]
fn ctrl_delete_over_a_pill_takes_only_the_pill() {
    let attr = Attr::new("due", AttrValue::Text("Friday".into()));
    let mut e = one_row("task");
    add_pill(&mut e, 0, &attr);
    e.editor.place_caret(5); // just before the anchor
    e.reflow();

    e.key(&named(NamedKey::Delete), None, PRIMARY);

    assert_eq!(text_of(&e), "task ");
}

/// Ticking a task marks it and **leaves it alone**.
///
/// A completed row used to sink to the end of its sibling group. The order of a
/// list is something the user arranged; ticking something off is not a request
/// to rearrange it, and the row jumping out from under the pointer was the
/// whole cost. `F4` already hides completed tasks for anyone who wants them
/// out of the way.
#[test]
fn ctrl_enter_ticks_the_row_and_leaves_it_where_it_is() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let id = e.body().focused_id().unwrap();
    let was = e.ws.sibling_index(id).unwrap();
    assert!(!e.body().focused().unwrap().done, "starts unticked");

    assert!(e.key(&named(NamedKey::Enter), None, PRIMARY));

    assert!(e.ws.read(id).unwrap().meta.done);
    assert_eq!(e.ws.sibling_index(id).unwrap(), was, "it did not move");
    assert_eq!(e.body().focus, 2, "and the caret is still on it");
}

/// Backspacing over a pill takes the whole thing — anchor and attribute —
/// because the anchor is one cluster.
#[test]
fn backspacing_a_pill_removes_its_attribute() {
    let mut e = outline();
    add_pill(
        &mut e,
        2,
        &Attr::labelled(
            "link",
            AttrValue::Url("https://koru.example.com/terms".into()),
            "Terms page",
        ),
    );
    e.body_mut().focus = 2;
    let id = e.body().focused_id().unwrap();
    let before = e.ws.read(id).unwrap().content.attrs().len();
    assert_eq!(before, 1);

    e.key(&named(NamedKey::End), None, NONE);
    e.key(&named(NamedKey::Backspace), None, NONE);
    assert_eq!(
        e.ws.read(id).unwrap().content.attrs().len(),
        before - 1,
        "the attribute went with the anchor"
    );
}

// ------------------------------------------------------------ undo

#[test]
fn ctrl_z_undoes_typing_and_ctrl_shift_z_puts_it_back() {
    let mut e = one_row("Reward");
    e.key(&named(NamedKey::End), None, NONE);
    e.key(&ch("!"), Some("!"), NONE);
    assert_eq!(text_of(&e), "Reward!");

    assert!(e.key(&ch("z"), None, PRIMARY));
    assert_eq!(text_of(&e), "Reward", "the keystroke came back out");

    let redo = koru_keys::PRIMARY | ModifiersState::SHIFT;
    assert!(e.key(&ch("z"), None, redo));
    assert_eq!(text_of(&e), "Reward!", "and went back in");
}

/// A structural change is its own step, so undo does not swallow the typing
/// that came before it — and the caret returns to the split point, not the
/// neighbour that inherited the vanished tail row's index.
#[test]
fn undo_reverses_a_split() {
    // A following row is load-bearing: without it, rebuild's focus clamp lands
    // on the only survivor by luck and the bug (caret left on the next line)
    // never shows.
    let mut e = rows_at(&[("Reward payout", 0), ("next line", 0)]);
    e.editor.place_caret(6);
    e.reflow();
    let was = e.body().focused_id().unwrap();
    e.key(&named(NamedKey::Enter), None, NONE);
    assert_eq!(e.body().rows.len(), 3);

    e.key(&ch("z"), None, PRIMARY);
    assert_eq!(e.body().rows.len(), 2, "the row is whole again");
    assert_eq!(e.body().rows[0].text, "Reward payout");
    assert_eq!(
        e.body().focused_id(),
        Some(was),
        "caret back on the rejoined row, not the neighbour"
    );
    assert_eq!(
        e.body().focused().unwrap().caret(),
        6,
        "and at the split point"
    );
}

#[test]
fn undo_reverses_a_reorder() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let moved = e.body().focused_id().unwrap();
    e.key(
        &named(NamedKey::ArrowUp),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    );
    assert_eq!(e.body().rows[1].id, moved);

    e.key(&ch("z"), None, PRIMARY);
    assert_eq!(e.body().rows[2].id, moved, "back where it started");
}

/// The seeded document is what you opened on, not an edit — Ctrl+Z on a
/// fresh editor must not empty the screen.
#[test]
fn undo_cannot_erase_the_document_you_opened_on() {
    let mut e = outline();
    let before: Vec<_> = e.body().rows.iter().map(|r| r.text.clone()).collect();
    for _ in 0..10 {
        e.key(&ch("z"), None, PRIMARY);
    }
    let after: Vec<_> = e.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(before, after);
}

// ------------------------------------------------------------ active line

/// Through `Leaf`, not `TaskOutline` — the outline's own tests cover the
/// behaviour, and this one covers that the shell's dispatch reaches it rather
/// than falling through to the shared handler, which deselects instead.
#[test]
fn clicking_below_every_row_lands_in_the_last_one() {
    let mut e = outline();
    assert!(e.view().active, "starts ready to type");
    // Far below the last row, and still on the page.
    let below = rowlayout::content_height(e.body()) + 200.0;
    e.mouse_down(300.0, below, false);
    assert!(e.view().active, "the empty page belongs to the last row");
    assert_eq!(e.body().focus, e.body().rows.len() - 1);
    assert!(e.body().focused().unwrap().at_end());
}

#[test]
fn clicking_a_row_activates_it_again() {
    let mut e = outline();
    e.deactivate();
    assert!(!e.view().active);
    let row = &e.body().rows[0];
    let (x, y) = (row.text_x() + 20.0, row.y + row.height * 0.5);
    e.mouse_down(x, y, false);
    assert!(e.view().active, "clicking a row re-activates the caret");
    assert_eq!(e.body().focus, 0);
}

#[test]
fn deactivating_collapses_the_selection() {
    let mut e = outline();
    e.key(&ch("a"), Some("a"), NONE);
    e.key(&ch("a"), None, PRIMARY); // select all in the line
    assert!(!e.body().focused().unwrap().selection.is_collapsed());
    e.deactivate();
    assert!(
        e.body().focused().unwrap().selection.is_collapsed(),
        "no caret means no selection"
    );
}

/// The *row* selection goes too. It is drawn whether or not a line is active,
/// so leaving it behind left rows visibly highlighted while every key that acts
/// on them — Ctrl+X, Ctrl+D, Tab — was being refused for having no caret.
#[test]
fn deactivating_drops_the_row_selection_too() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert!(e.body().has_row_selection(), "two rows selected");

    e.deactivate();

    assert!(!e.body().has_row_selection());
}

/// The gesture that produces the state, rather than the method call.
///
/// This used to click the status bar, on the grounds that it was chrome rather
/// than a row. There is no status bar now — a document has no footer, and that
/// band is the end of the page — so the gesture is the one that band became:
/// a press in the empty space below the last row, which appends a paragraph and
/// puts the caret in it. Either way the caret has moved somewhere the selected
/// rows are not, and leaving them highlighted would aim the next Ctrl+X at rows
/// nobody is looking at.
#[test]
fn clicking_below_the_rows_drops_the_row_selection() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert!(e.body().has_row_selection());

    let (_, vh) = e.view().viewport;
    e.mouse_down(300.0, vh - theme::STATUS_H / 2.0, false);

    assert!(!e.body().has_row_selection());
}

/// Typing with no active line must not silently edit the document.
#[test]
fn keys_do_nothing_while_inactive() {
    let mut e = outline();
    e.deactivate();
    let before: Vec<String> = e.body().rows.iter().map(|r| r.text.clone()).collect();
    for key in [ch("x"), named(NamedKey::Enter), named(NamedKey::Backspace)] {
        assert!(!e.key(&key, Some("x"), NONE), "{key:?} should be ignored");
    }
    let after: Vec<String> = e.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(before, after, "document untouched while inactive");
}

/// Hide-completed is not line-specific, so it keeps working with no active line.
#[test]
fn global_toggles_still_work_while_inactive() {
    let mut e = outline();
    e.deactivate();
    assert!(e.key(&named(NamedKey::F4), None, NONE));
    assert!(e.body().hide_completed);
}

#[test]
fn link_at_finds_only_link_pills() {
    let mut e = outline();
    add_pill(
        &mut e,
        2,
        &Attr::labelled(
            "link",
            AttrValue::Url("https://koru.example.com/terms".into()),
            "Terms page",
        ),
    );
    // A pill that is not a link, so "only" in the name is actually tested.
    add_pill(
        &mut e,
        3,
        &Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            )),
        ),
    );

    // Find it through the same geometry the renderer uses.
    add_pill(
        &mut e,
        2,
        &Attr::labelled(
            "link",
            AttrValue::Url("https://koru.example.com/terms".into()),
            "Terms page",
        ),
    );
    let mut found: Option<String> = None;
    for row in &e.body().rows {
        rowlayout::visit_pills(row, &e.view().engine.pills, |_, info, _, px, py, pw, ph| {
            if info.kind == PillKind::Link {
                let x = row.text_x() + px + pw * 0.5;
                let y = row.y + py + ph * 0.5;
                found = e.link_at(x, y).map(str::to_owned);
            }
        });
    }
    assert_eq!(found.as_deref(), Some("https://koru.example.com/terms"));
}

#[test]
fn hover_reports_the_row_it_belongs_to() {
    let attr = koru_core::AttrId::new();
    assert_eq!(Hover::Grip(3).row(), Some(3));
    assert_eq!(Hover::Checkbox(1).row(), Some(1));
    assert_eq!(Hover::Text(4).row(), Some(4));
    assert_eq!(Hover::Pill { row: 2, attr }.row(), Some(2));
    // Panel rows are not document rows, so they must not highlight one.
    assert_eq!(Hover::PanelRow(0).row(), None);
}

/// An `@` document pill stores stable identity, and a newer manifest

/// **A deep-link caption is two live halves.** The page can be renamed and the
/// heading retyped independently, and the pill has to follow both -- which is
/// why the caption is composed from the cache rather than stored joined.
#[test]
fn a_deep_link_pill_caption_follows_both_halves() {
    let mut e = one_prose_row("See ");
    let document = DocumentId::new();
    let node = koru_core::NodeId::new();
    let attr = koru_core::Attr::labelled(
        "document",
        koru_core::AttrValue::Document(koru_core::DocumentReference::at(document, node)),
        "Spec#Rollout".to_owned(),
    );
    add_pill(&mut e, 0, &attr);

    let key = format!("{}#{}", document.to_storage(), node.to_storage());
    // The host composes the two halves; the editor is handed the answer.
    e.editor.set_entity_titles(
        &std::collections::HashMap::from([(key.clone(), "Roadmap#Rollback".to_string())]),
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    );

    let row = pill_row(&e);
    let info = e.body().rows[row].pills.values().next().unwrap().clone();
    assert_eq!(info.label, "Roadmap#Rollback");
    assert!(!info.trashed);

    // **Trash belongs to the document.** The bin never keys on a block, so a
    // lookup with the whole composite key would answer false forever.
    e.editor.set_entity_titles(
        &std::collections::HashMap::from([(key, "Roadmap#Rollback".to_string())]),
        &std::collections::HashSet::from([document.to_storage()]),
        &std::collections::HashSet::from([document.to_storage()]),
    );
    assert!(
        e.body().rows[row].pills.values().next().unwrap().trashed,
        "a deep link into a binned document says so"
    );
}

/// **A caption refresh must not blank the pills it did not rename.** Only the
/// changed rows are dirtied and reflow rebuilds dirty rows only, so a pill
/// visual dropped from the cache for an *unchanged* row is never measured
/// again — the inline box keeps its width and the pill silently stops
/// drawing. `set_entity_titles` therefore may not clear the cache; a renamed
/// caption is a new cache key and measures fresh on its own.
#[test]
fn a_caption_refresh_keeps_unrenamed_pills_drawable() {
    let mut e = rows_at(&[("one ", 0), ("two ", 0)]);
    let renamed = DocumentId::new();
    let kept = DocumentId::new();
    let node = koru_core::NodeId::new();
    add_pill(
        &mut e,
        0,
        &koru_core::Attr::labelled(
            "document",
            koru_core::AttrValue::Document(koru_core::DocumentReference::at(renamed, node)),
            "Old".to_owned(),
        ),
    );
    add_pill(
        &mut e,
        1,
        &koru_core::Attr::labelled(
            "document",
            koru_core::AttrValue::Document(koru_core::DocumentReference::at(kept, node)),
            "Stays".to_owned(),
        ),
    );

    let key = format!("{}#{}", renamed.to_storage(), node.to_storage());
    e.editor.set_entity_titles(
        &std::collections::HashMap::from([(key, "New".to_string())]),
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    );

    let mut drawable = Vec::new();
    for row in &e.body().rows {
        rowlayout::visit_pills(row, &e.view().engine.pills, |_, info, _, _, _, _, _| {
            drawable.push(info.label.clone());
        });
    }
    assert!(
        drawable.contains(&"New".to_string()),
        "the renamed pill re-measures under its new caption"
    );
    assert!(
        drawable.contains(&"Stays".to_string()),
        "a pill in an untouched row keeps its visual"
    );
}

/// title remains visible through later refreshes of the containing row.
#[test]
fn a_document_pill_references_a_node_and_follows_its_title() {
    let mut e = one_prose_row("Write ");
    let doc = koru_workspace::document(&mut e.ws, "spec.md").expect("the node exists");
    let current = e.ws.id().expect("the workspace has an id");
    let profile = ProfileId::new();
    let document = DocumentId::from_uuid(doc.as_uuid());

    e.key(&named(NamedKey::End), None, NONE);
    e.key(&ch("@"), Some("@"), NONE);
    e.editor.set_entity_context(
        current,
        vec![koru_editor::components::entity_picker::WorkspaceChoice {
            profile,
            profile_name: "Profile".into(),
            id: current,
            name: "Workspace".into(),
        }],
    );
    e.editor.set_entity_search_results(
        vec![koru_editor::components::entity_picker::DocumentChoice {
            target: koru_editor::trigger::DocumentTarget {
                document,
                node: None,
            },
            title: "spec.md".into(),
            breadcrumb: "Workspace / spec.md".into(),
            is_file: false,
        }],
        false,
    );
    e.key(&named(NamedKey::Enter), None, NONE);

    let row = e.body().focused().unwrap();
    let info = row.pills.values().next().expect("a pill was inserted");
    assert_eq!(info.label, "spec.md");
    let source = e.ws.read(row.id).unwrap();
    let reference = source.content.attrs()[0]
        .as_document_reference()
        .expect("the pill stores a stable document reference");
    assert_eq!(reference.document, document);
    assert_eq!(reference.node, None);

    // A manifest/search update supplies the renamed title. Re-reading the
    // source row must not restore the old caption cached in its attribute.
    e.ws.set_text(doc, "reward-spec.md").unwrap();
    e.editor.set_entity_titles(
        &std::collections::HashMap::from([(document.to_storage(), "reward-spec.md".to_string())]),
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    );
    let id = e.body().focused().unwrap().id;
    e.editor.body_mut().refresh(&e.ws, id);
    let info = e.body().focused().unwrap().pills.values().next().unwrap();
    assert_eq!(info.label, "reward-spec.md");

    let key = document.to_storage();
    e.editor.set_entity_titles(
        &std::collections::HashMap::from([(key.clone(), "reward-spec.md".to_string())]),
        &std::collections::HashSet::from([key.clone()]),
        &std::collections::HashSet::from([key]),
    );
    assert!(
        e.body()
            .focused()
            .unwrap()
            .pills
            .values()
            .next()
            .unwrap()
            .trashed,
        "the routed document now wears the Trash icon"
    );
    e.editor.body_mut().refresh(&e.ws, id);
    assert!(
        e.body()
            .focused()
            .unwrap()
            .pills
            .values()
            .next()
            .unwrap()
            .trashed,
        "an ordinary row refresh keeps manifest lifecycle metadata"
    );
}

/// Add new page waits for the Store's real id, then replaces the complete
/// `@query` with that routed pill rather than merely opening an unlinked page.
#[test]
fn add_new_page_completes_the_authored_query_as_a_document_pill() {
    let mut e = one_prose_row("Write ");
    let document = DocumentId::new();

    e.key(&named(NamedKey::End), None, NONE);
    e.key(&ch("@"), Some("@"), NONE);
    e.key(&ch("f"), Some("Fresh page"), NONE);
    e.key(&named(NamedKey::Enter), None, NONE);

    assert_eq!(e.editor.take_wants_new_page(), Some("Fresh page".into()));
    let row = e.body().focused().unwrap();
    assert_eq!(
        e.ws.read(row.id).unwrap().content.as_text(),
        Some("Write @Fresh page"),
        "the query remains until the Store supplies a real id"
    );

    assert!(e.complete_new_page_link(
        "Fresh page".into(),
        koru_editor::trigger::DocumentTarget {
            document,
            node: None,
        },
    ));
    let row = e.body().focused().unwrap();
    let info = row.pills.values().next().expect("the linked page pill");
    assert_eq!(info.label, "Fresh page");
    let source = e.ws.read(row.id).unwrap();
    assert_eq!(source.content.as_text(), Some("Write \u{2060} "));
    assert_eq!(
        source.content.attrs()[0]
            .as_document_reference()
            .map(|reference| reference.document),
        Some(document)
    );
    assert_eq!(
        source.content.attrs()[0]
            .as_document_reference()
            .and_then(|reference| reference.node),
        None
    );
}

/// Two tasks naming the same document share one node, which is what makes
/// "every task related to this document" answerable.
#[test]
fn two_tasks_naming_one_document_share_it() {
    let mut e = one_row("first");
    let a = koru_workspace::document(&mut e.ws, "spec.md").unwrap();
    let b = koru_workspace::document(&mut e.ws, "spec.md").unwrap();
    assert_eq!(a, b, "found, not created twice");

    let other = koru_workspace::document(&mut e.ws, "notes.md").unwrap();
    assert_ne!(a, other);
}

/// A new document opens on something typable rather than a bare title.
#[test]
fn a_new_document_starts_with_an_empty_paragraph() {
    let mut e = one_row("x");
    let doc = koru_workspace::document(&mut e.ws, "spec.md").unwrap();
    let body = e.ws.walk_under(doc).unwrap();
    assert_eq!(body.len(), 1);
    let (child, depth) = body[0];
    assert_eq!(depth, 0);
    assert_eq!(e.ws.read(child).unwrap().kind, koru_core::Kind::Paragraph);
}

// -------------------------------------------------------- the document view

/// A document view is the same editor rooted at the document node, so its
/// rows are the document's body and nothing else.
#[test]
fn a_document_view_shows_the_documents_body() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    h.ws.insert(
        koru_core::schema::Parent::Node(doc),
        koru_core::node::Node::paragraph("Overview").unwrap(),
    )
    .unwrap();

    let view = koru_document::BuiltInDocument::open(&h.ws, doc);

    let texts: Vec<_> = view.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(
        texts,
        vec![
            "spec.md".to_string(),
            "".to_string(),
            "Overview".to_string()
        ],
        "the document's own name is row 0, and its children follow"
    );
    assert!(
        view.body().rows[0].is_title,
        "row 0 is the title, not a paragraph that happens to read like one"
    );
    assert_eq!(view.body().rows[0].id, doc);
    assert_eq!(
        view.body().focus,
        BODY,
        "the caret opens in the body, not in the name"
    );
    assert!(
        view.body().rows.iter().all(|r| !r.is_task()),
        "a document's body is paragraphs, so no checkboxes"
    );
}

/// Clicking a document pill asks the shell for a window rather than
/// re-opening the rename prompt — the title is edited in the document.
#[test]
fn clicking_a_document_pill_asks_to_open_it() {
    let mut e = outline();
    let doc = koru_workspace::document(&mut e.ws, "spec.md").unwrap();
    add_pill(
        &mut e,
        1,
        &Attr::new("document", AttrValue::Ref(Ref::new(doc))),
    );

    let (row, attr) = e
        .body()
        .rows
        .iter()
        .enumerate()
        .find_map(|(i, r)| {
            r.pills
                .iter()
                .find(|(_, info)| info.kind == PillKind::Document)
                .map(|(a, _)| (i, *a))
        })
        .expect("the document pill added above");

    // Click its centre, through the same geometry the renderer uses.
    let r = &e.body().rows[row];
    let ox = r.text_x();
    let oy = r.y;
    let mut point = None;
    rowlayout::visit_pills(r, &e.view().engine.pills, |a, _, _, px, py, pw, ph| {
        if a == attr {
            point = Some((ox + px + pw * 0.5, oy + py + ph * 0.5));
        }
    });
    let (x, y) = point.expect("the pill has a rect");

    assert!(e.mouse_down(x, y, false));
    assert_eq!(
        e.view().wants_open.map(|(_, where_)| where_),
        Some(koru_editor::OpenWhere::SameTab),
        "a click follows in this tab"
    );
    assert!(
        !e.view().overlay.is_open(),
        "and did not open a rename prompt"
    );
}

/// The toolbar and the key map must not drift: both call the same methods.
#[test]
fn a_toolbar_button_does_what_its_chord_does() {
    use koru_document::docview::Command as Tool;

    let mut bykey = with_selection("a bcd e", 2..5);
    bykey.key(&ch("b"), None, PRIMARY);

    let mut bytool = with_selection("a bcd e", 2..5);
    bytool.editor.act(&mut bytool.ws, Tool::Bold.action());

    assert_eq!(marked(&bykey, Mark::Bold), "bcd");
    assert_eq!(marked(&bytool, Mark::Bold), "bcd");
    assert_eq!(text_of(&bykey), text_of(&bytool));
}

/// The same, for a block property — a heading button and its chord are the one
/// action, so neither can grow a behaviour the other lacks.
#[test]
fn a_heading_button_does_what_its_chord_does() {
    use koru_document::docview::Command as Tool;

    let mut bykey = a_document();
    bykey.key(&ch("3"), None, PRIMARY_ALT);

    let mut bytool = a_document();
    bytool.editor.act(&mut bytool.ws, Tool::H3.action());
    bytool.editor.reflow();

    assert_eq!(bykey.body().rows[BODY].heading, Some(Heading::H3));
    assert_eq!(bytool.body().rows[BODY].heading, Some(Heading::H3));
    assert_eq!(text_of(&bykey), text_of(&bytool));
}

/// The toolbar reads the level off the focused row, so the button that matches
/// what the caret is sitting in draws pressed and the others do not.
#[test]
fn the_toolbar_reflects_the_heading_under_the_caret() {
    use koru_document::docview::{Command as Tool, State};

    let mut h = a_document();
    h.key(&ch("2"), None, PRIMARY_ALT);

    let state = State::read(&h.ws, h.body(), h.body().armed());
    assert!(state.is_active(Tool::H2));
    assert!(!state.is_active(Tool::H1));
    assert!(!state.is_active(Tool::H3));
}

/// Superscript and subscript are one value, so choosing one replaces the
/// other rather than stacking.
#[test]
fn superscript_and_subscript_are_mutually_exclusive() {
    use koru_document::docview::Command as Tool;
    let mut e = with_selection("x2", 1..2);

    e.editor.act(&mut e.ws, Tool::Superscript.action());
    let script = |e: &Harness| {
        e.body()
            .focused()
            .unwrap()
            .runs
            .iter()
            .find_map(|r| r.style.script)
    };
    assert_eq!(script(&e), Some(koru_core::Script::Super));

    e.editor.act(&mut e.ws, Tool::Subscript.action());
    assert_eq!(
        script(&e),
        Some(koru_core::Script::Sub),
        "replaced, not stacked"
    );

    e.editor.act(&mut e.ws, Tool::Subscript.action());
    assert_eq!(script(&e), None, "pressing it again turns it off");
}

/// The toolbar reports what is under the caret, which is what makes a
/// button look pressed at the right moment.
#[test]
fn the_toolbar_reflects_the_marks_under_the_caret() {
    use koru_document::docview::{Command as Tool, State};
    let mut e = with_selection("a bcd e", 2..5);
    e.key(&ch("b"), None, PRIMARY);

    let state = State::read(&e.ws, e.body(), e.body().armed());
    assert!(state.is_active(Tool::Bold));
    assert!(!state.is_active(Tool::Italic));
    assert!(!state.is_active(Tool::AlignCenter));

    e.editor.act(&mut e.ws, Tool::AlignCenter.action());
    let state = State::read(&e.ws, e.body(), e.body().armed());
    assert!(state.is_active(Tool::AlignCenter));
}

/// Chrome above the rows pushes them down. A document tab has none now, but a
/// window that draws its own caption does — and `top_inset` is what carries it.
#[test]
fn chrome_pushes_the_rows_down() {
    let mut plain = one_row("row");
    plain.editor.reflow();
    let without = plain.body().rows[0].y;

    plain.editor.view_mut().chrome_h = theme::CAPTION_H;
    plain.editor.reflow();
    assert!(plain.body().rows[0].y >= without + theme::CAPTION_H);
}

/// Double-clicking the document's name opens a prompt seeded with it, so
/// typing replaces rather than appends.
#[test]
fn double_clicking_the_title_renames_the_document() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();

    assert!(
        h.editor.start_rename(&h.ws),
        "F2 opens the rename prompt on a view that has a root"
    );
    assert!(matches!(h.view().overlay, Overlay::Name(_)));
    // Opening is not the same as being visible: the panel is drawn from
    // `overlay_panel_rect`, and a prompt with no rect is a prompt nobody
    // can see.
    let rect = h
        .editor
        .overlay_panel_rect()
        .expect("the rename prompt has somewhere to draw");
    assert!(rect.w > 0.0 && rect.h > 0.0, "and it has a size");
    assert!(
        rect.y >= h.body().rows[0].y,
        "it drops below the title row rather than over it"
    );

    // Type a new name over the selected old one and commit.
    for c in "reward.md".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    h.key(&named(NamedKey::Enter), None, NONE);

    assert_eq!(
        h.ws.read(doc).unwrap().content.as_text(),
        Some("reward.md"),
        "the document's own text changed"
    );
    assert!(!h.view().overlay.is_open());
}

/// **Every overlay that opens must have somewhere to draw.**
///
/// `draw_overlay` bails when `overlay_panel_rect` is `None`, silently — so a
/// prompt anchored to something that is not a row opens, accepts keys, and
/// is invisible. That is exactly what the rename prompt did: it anchored to
/// the document node, which is the zoom root and therefore deliberately not
/// a row.
#[test]
fn no_overlay_can_open_with_nowhere_to_draw() {
    // Ctrl+K, on an ordinary row.
    let mut e = one_row("a line");
    e.key(&ch("k"), None, PRIMARY);
    assert!(e.view().overlay.is_open());
    assert!(
        e.editor.overlay_panel_rect().is_some(),
        "the link prompt has a rect"
    );

    // The slash menu, and the pickers it leads to.
    let mut e = one_row("");
    e.key(&ch("/"), Some("/"), NONE);
    assert!(e.view().overlay.is_open());
    assert!(
        e.editor.overlay_panel_rect().is_some(),
        "the command menu has a rect"
    );
    // Select a command that opens a picker explicitly. The alphabetic first
    // row may be a direct shell request (`/attach`), which correctly has no
    // overlay of its own. `du` selects `/due` rather than `/dictate` — the
    // same month grid, armed for `koru.due`.
    e.key(&ch("d"), Some("d"), NONE);
    e.key(&ch("u"), Some("u"), NONE);
    e.key(&named(NamedKey::Enter), None, NONE); // /due
    assert!(
        e.editor.overlay_panel_rect().is_some(),
        "the date grid has a rect"
    );

    // The rename prompt, anchored to the toolbar rather than to a row.
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();
    h.editor.start_rename(&h.ws);
    assert!(h.view().overlay.is_open());
    assert!(
        h.editor.overlay_panel_rect().is_some(),
        "the rename prompt has a rect"
    );
}

/// Highlight is a toggle you can arm: press it, type, and what you typed
/// comes out highlighted. Same shape as bold, and for the same reason — a
/// mark cannot mark an empty range, so the intent has to be remembered.
#[test]
fn highlight_armed_at_a_caret_applies_to_what_is_typed_next() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("");
    e.editor.act(&mut e.ws, Tool::Highlight.action());
    e.key(&ch("l"), Some("lit"), NONE);

    let id = e.body().rows[0].id;
    assert_eq!(text_of(&e), "lit");
    assert_eq!(
        e.ws.style_in(id, 0..3).unwrap().highlight.as_deref(),
        Some(theme::highlight_names()[0])
    );
}

/// And the button looks pressed before there is a character to carry it,
/// or the toolbar would contradict what typing is about to do.
#[test]
fn the_toolbar_shows_an_armed_highlight() {
    use koru_document::docview::{Command as Tool, State};
    let mut e = one_row("");
    assert!(!State::read(&e.ws, e.body(), e.body().armed()).is_active(Tool::Highlight));

    e.editor.act(&mut e.ws, Tool::Highlight.action());
    let state = State::read(&e.ws, e.body(), e.body().armed());
    assert!(state.is_active(Tool::Highlight));
    assert_eq!(
        state.style.highlight.as_deref(),
        Some(theme::highlight_names()[0])
    );
}

/// Pressing it again cycles the *armed* colour. Reading the document
/// instead would find no highlight and hand back the first swatch forever.
#[test]
fn arming_highlight_twice_cycles_to_the_next_colour() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("");
    e.editor.act(&mut e.ws, Tool::Highlight.action());
    e.editor.act(&mut e.ws, Tool::Highlight.action());
    e.key(&ch("x"), Some("x"), NONE);

    let id = e.body().rows[0].id;
    assert_eq!(
        e.ws.style_in(id, 0..1).unwrap().highlight.as_deref(),
        Some(theme::highlight_names()[1])
    );
}

/// Cycling past the last swatch arms nothing — with no selection to clear,
/// the same key has to be the way back off.
#[test]
fn cycling_an_armed_highlight_past_the_end_turns_it_off() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("");
    for _ in 0..theme::highlight_names().len() + 1 {
        e.editor.act(&mut e.ws, Tool::Highlight.action());
    }
    e.key(&ch("x"), Some("x"), NONE);

    let id = e.body().rows[0].id;
    assert_eq!(e.ws.style_in(id, 0..1).unwrap().highlight, None);
}

/// Moving the caret disarms it, like every other pending mark — otherwise a
/// stray press would colour text somewhere the user has stopped looking.
#[test]
fn an_armed_highlight_is_dropped_when_the_caret_moves() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("ab");
    e.editor.act(&mut e.ws, Tool::Highlight.action());
    e.key(&Key::Named(NamedKey::ArrowRight), None, NONE);
    e.key(&ch("x"), Some("x"), NONE);

    let id = e.body().rows[0].id;
    let at = text_of(&e).find('x').unwrap();
    assert_eq!(e.ws.style_in(id, at..at + 1).unwrap().highlight, None);
}

/// Superscript arms the same way, because it is the same mechanism — the
/// point of value marks being one thing rather than three.
#[test]
fn superscript_armed_at_a_caret_applies_to_what_is_typed_next() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("x");
    e.editor.place_caret(1);
    e.reflow();
    e.editor.act(&mut e.ws, Tool::Superscript.action());
    e.key(&ch("2"), Some("2"), NONE);

    let id = e.body().rows[0].id;
    assert_eq!(
        e.ws.style_in(id, 1..2).unwrap().script,
        Some(koru_core::Script::Super)
    );
    assert_eq!(
        e.ws.style_in(id, 0..1).unwrap().script,
        None,
        "the letter before it stayed on the baseline"
    );
}

/// A document is prose, so Tab must not nest one paragraph under another —
/// which is what made every line after it inherit the indent.
#[test]
fn tab_does_not_indent_inside_a_document() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);

    let first = h.body().rows[BODY].id;
    h.ws.insert_text(first, 0, "Overview").unwrap();
    h.editor.body_mut().refresh(&h.ws, first);
    h.editor.reflow();

    h.key(&named(NamedKey::Tab), None, NONE);
    h.key(&named(NamedKey::Enter), None, NONE);
    h.key(&ch("s"), Some("second"), NONE);

    assert!(
        h.body().rows.iter().all(|r| r.depth == 0),
        "every line starts at the left margin"
    );
    assert!(
        matches!(h.ws.parent(h.body().rows[1].id), Ok(Parent::Node(n)) if n == doc),
        "and the second line hangs off the document, not off the first line"
    );
}

/// The boundary itself: the shared editor refuses to perform structure, so a
/// view that never claims it cannot get it by accident. `TaskOutline::act`
/// claims it; `Editor::act` says no.
#[test]
fn the_shared_editor_refuses_structural_actions() {
    use koru_editor::input::action::Action;

    let mut h = outline();
    let before: Vec<_> = h.body().rows.iter().map(|r| r.depth).collect();

    for action in [Action::Indent(1), Action::Reorder(-1), Action::ToggleDone] {
        assert!(
            !h.editor.act(&mut h.ws, action),
            "{action:?} is the outline's, not the editor's"
        );
    }
    let after: Vec<_> = h.body().rows.iter().map(|r| r.depth).collect();
    assert_eq!(before, after, "and refusing means doing nothing");

    // The view does claim them, so the chord still works end to end.
    h.editor.body_mut().focus = 2;
    assert!(h.key(&named(NamedKey::Tab), None, NONE));
    assert!(h.body().rows[2].depth > before[2], "Tab still indents");
}

/// A document gives the outline's completion chord a prose meaning: make a
/// complete paragraph below without splitting or ticking the current one.
#[test]
fn ctrl_enter_on_document_prose_opens_a_paragraph_below() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    let first = h.body().rows[BODY].id;
    h.ws.insert_text(first, 0, "Overview").unwrap();
    h.editor.body_mut().refresh(&h.ws, first);
    h.editor.reflow();

    assert!(
        h.key(&named(NamedKey::Enter), None, PRIMARY),
        "swallowed, not passed on"
    );
    let after: Vec<_> = h.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(after, ["spec.md", "Overview", ""]);
    assert_eq!(h.body().focused().map(|row| row.text.as_str()), Some(""));
    assert!(
        !h.ws.read(first).unwrap().meta.done,
        "Ctrl+Enter did not tick a paragraph"
    );
}

/// Ctrl+Shift+↑/↓ moves document blocks among their siblings — the same chord
/// the outline uses, and the keyboard twin of the drag grip.
#[test]
fn ctrl_shift_reorders_document_blocks() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    // Two named body paragraphs under the title (create_document also seeds an
    // empty first line — order assertions key off the ids, not absolute indices).
    let a =
        h.ws.insert(
            koru_core::schema::Parent::Node(doc),
            koru_core::node::Node::paragraph("alpha").unwrap(),
        )
        .unwrap();
    let b =
        h.ws.insert(
            koru_core::schema::Parent::Node(doc),
            koru_core::node::Node::paragraph("beta").unwrap(),
        )
        .unwrap();
    as_document(&mut h, doc);
    h.body_mut().focus = h.body().index_of(a).expect("alpha is a row");
    let mods = koru_keys::PRIMARY.union(ModifiersState::SHIFT);

    assert!(h.key(&named(NamedKey::ArrowDown), None, mods));
    assert!(
        h.body().index_of(b).unwrap() < h.body().index_of(a).unwrap(),
        "alpha moved below beta"
    );
    assert_eq!(h.body().focused_id(), Some(a), "focus followed the block");

    assert!(h.key(&named(NamedKey::ArrowUp), None, mods));
    assert!(
        h.body().index_of(a).unwrap() < h.body().index_of(b).unwrap(),
        "and back"
    );

    // The title is the document; it does not move among its children.
    h.body_mut().focus = 0;
    let title = h.body().rows[0].id;
    let before: Vec<_> = h.body().rows.iter().map(|r| r.id).collect();
    assert!(h.key(&named(NamedKey::ArrowDown), None, mods));
    let after: Vec<_> = h.body().rows.iter().map(|r| r.id).collect();
    assert_eq!(before, after, "title stays put");
    assert_eq!(h.body().rows[0].id, title);
}

/// It types a tab instead. Nesting is an outline idea, but Tab still has to
/// do the obvious thing rather than nothing.
#[test]
fn tab_inside_a_document_types_a_tab() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);

    h.key(&named(NamedKey::Tab), None, NONE);
    h.key(&ch("a"), Some("after"), NONE);
    assert_eq!(text_of(&h), "\tafter");

    // Shift+Tab has no outdent to do, and must not type anything either.
    h.key(&named(NamedKey::Tab), None, ModifiersState::SHIFT);
    assert_eq!(text_of(&h), "\tafter");
}

/// The feature rests on parley giving a tab an advance — it is a single
/// advance rather than a tab stop, but it is not zero, so an indented line
/// reads as indented. Measured, because nothing documents it.
#[test]
fn parley_gives_a_tab_more_width_than_a_space() {
    let mut e = koru_draw::text::TextEngine::new();
    let mut w = |t: &str| e.simple_layout(t, theme::role::body()).width();
    assert!(
        w("a\tb") > w("a b"),
        "a tab is wider than a space, so it shows"
    );
}

/// Nesting a document picked up before the rule existed is *shown* flat
/// rather than rewritten — a projection, not a migration.
#[test]
fn a_document_shows_existing_nesting_flat() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    let parent =
        h.ws.insert(Parent::Node(doc), Node::paragraph("parent").unwrap())
            .unwrap();
    let child =
        h.ws.insert(Parent::Node(parent), Node::paragraph("child").unwrap())
            .unwrap();

    as_document(&mut h, doc);
    assert!(
        h.body().rows.iter().all(|r| r.depth == 0),
        "the view flattens it"
    );
    assert!(
        matches!(h.ws.parent(child), Ok(Parent::Node(n)) if n == parent),
        "but the document itself is untouched"
    );
}

/// Clearing a value mark is not a no-op — it disturbs a neighbouring pill's
/// span, which is how `typing_beside_a_pill_does_not_join_it` was broken
/// once before. So armed formatting must only write what actually differs
/// from what the new text inherits.
#[test]
fn armed_formatting_that_changes_nothing_leaves_a_neighbouring_pill_alone() {
    use koru_document::docview::Command as Tool;
    let mut e = one_row("");
    let id = e.body().rows[0].id;
    let target =
        e.ws.insert(Parent::Root, Node::task("spec.md").unwrap())
            .unwrap();
    let attr = koru_core::Attr::new(
        "document",
        koru_core::AttrValue::Ref(koru_core::Ref::new(target)),
    );
    e.ws.insert_pill(id, 0, &attr).unwrap();
    e.editor.body_mut().refresh(&e.ws, id);

    // Cycle highlight all the way round, so what is armed is "no highlight"
    // — a pending style that changes nothing. Then type beside the pill.
    e.editor.place_caret(e.ws.text_len(id).unwrap());
    e.reflow();
    for _ in 0..theme::highlight_names().len() + 1 {
        e.editor.act(&mut e.ws, Tool::Highlight.action());
    }
    e.key(&ch("x"), Some("x"), NONE);

    let pills = e
        .body()
        .rows
        .iter()
        .flat_map(|r| r.runs.iter())
        .filter(|r| r.pill.is_some())
        .count();
    assert_eq!(pills, 1, "the pill is still exactly one run");
    assert!(text_of(&e).ends_with('x'));
}

/// Clicking the page below the last line carries on writing, rather than
/// dropping the caret the way clicking empty outline does.
#[test]
fn clicking_below_a_document_lands_on_a_line_to_type() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();

    let rows_before = h.body().rows.len();
    let below = rowlayout::content_height(h.body()) + 120.0;
    assert!(h.mouse_down(300.0, below - h.view().scroll, false));

    assert!(h.view().active, "ready to type");
    assert_eq!(
        h.body().rows.len(),
        rows_before,
        "the seeded document already ends in an empty line, so none was added"
    );
    assert_eq!(h.body().focus, h.body().rows.len() - 1);
}

/// A second click must not stack blank lines.
#[test]
fn clicking_below_twice_does_not_stack_empty_lines() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();

    // Give the last line some text, so the first click has to add one.
    let last = h.body().rows.last().unwrap().id;
    h.ws.insert_text(last, 0, "Overview").unwrap();
    h.editor.body_mut().refresh(&h.ws, last);
    h.editor.reflow();
    let before = h.body().rows.len();

    let below = rowlayout::content_height(h.body()) + 120.0;
    h.mouse_down(300.0, below - h.view().scroll, false);
    assert_eq!(h.body().rows.len(), before + 1, "one line added");

    let below = rowlayout::content_height(h.body()) + 120.0;
    h.mouse_down(300.0, below - h.view().scroll, false);
    assert_eq!(
        h.body().rows.len(),
        before + 1,
        "the second click reuses the empty line it just made"
    );
}

/// A new line in a document is a paragraph, not a task — it takes the kind
/// of the line above it.
#[test]
fn a_line_appended_in_a_document_is_a_paragraph() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();

    let last = h.body().rows.last().unwrap().id;
    h.ws.insert_text(last, 0, "Overview").unwrap();
    h.editor.body_mut().refresh(&h.ws, last);
    h.editor.reflow();

    let below = rowlayout::content_height(h.body()) + 120.0;
    h.mouse_down(300.0, below - h.view().scroll, false);

    let added = h.body().rows.last().unwrap();
    assert!(!added.is_task(), "a document line has no checkbox");
    assert_eq!(
        h.ws.read(added.id).unwrap().kind,
        koru_core::Kind::Paragraph
    );
}

/// The same gesture in the other view. Both take you back to typing, but only
/// the document makes a line to type on: an outline row is a *task*, and a
/// click on empty page must not invent one.
#[test]
fn clicking_below_the_outline_appends_nothing() {
    let mut e = outline();
    let rows = e.body().rows.len();
    let below = rowlayout::content_height(e.body()) + 200.0;
    e.mouse_down(300.0, below, false);
    assert_eq!(e.body().rows.len(), rows);
    assert_eq!(e.body().focus, rows - 1);
}

// ------------------------------------------------------ pasting into a view
//
// Through `paste_plain_text`, which takes its text as an argument and never
// needs clipboard acquisition. The shared mechanism has its own injected
// fixture coverage; this test is about document paste semantics.

/// A document's body is one flat sequence ([ADR-23]), so pasted lines are
/// siblings however they were indented.
///
/// The parent assertion is the one that matters. Depth is a projection —
/// `Body::rebuild` reports 0 for every row in a flat body — so the old code
/// passed a depth check while building real nesting underneath that nothing
/// could see and `Shift+Tab` refused to touch.
///
/// [ADR-23]: ../../../docs/decisions/README.md
#[test]
fn pasting_lines_into_a_document_keeps_them_flat() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);

    koru_editor::clipboard::paste_plain_text(&mut h.editor, &mut h.ws, "one\n\ttwo\n\tthree");

    assert!(h.body().rows.iter().all(|r| r.depth == 0), "drawn flat");
    // Row 0 is the document node itself — its parent is the library, not the
    // document — so the body proper starts after it.
    for row in &h.body().rows[BODY..] {
        assert!(
            matches!(h.ws.parent(row.id).unwrap(), Parent::Node(p) if p == doc),
            "and a child of the document itself, not of the line above"
        );
    }
}

/// Pasted lines take the kind of what they land beside, so text pasted into
/// prose does not arrive wearing checkboxes.
#[test]
fn pasted_lines_in_a_document_are_paragraphs_not_tasks() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);

    koru_editor::clipboard::paste_plain_text(&mut h.editor, &mut h.ws, "one\ntwo");

    for row in &h.body().rows[BODY..] {
        assert!(!row.is_task(), "no checkbox in prose");
        assert_eq!(h.ws.read(row.id).unwrap().kind, koru_core::Kind::Paragraph);
    }
}

/// In prose a tab is a tab (ADR-23), so pasted indentation is *text* rather
/// than structure. Reading it as structure deleted the character outright,
/// because a flat body then had nowhere to put the depth.
#[test]
fn a_pasted_indent_is_literal_text_in_prose() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);

    koru_editor::clipboard::paste_plain_text(&mut h.editor, &mut h.ws, "one\n\ttwo");

    let texts: Vec<&str> = h.body().rows.iter().map(|r| r.text.as_str()).collect();
    assert!(
        texts.contains(&"\ttwo"),
        "the tab survived as text: {texts:?}"
    );
}

/// The counterpart, so the two behaviours are stated side by side: an outline
/// *does* nest, so the same text arrives as structure there.
#[test]
fn pasted_lines_in_the_outline_become_nested_tasks() {
    let mut e = rows_at(&[("here", 0)]);

    koru_editor::clipboard::paste_plain_text(&mut e.editor, &mut e.ws, "one\n\ttwo\n\tthree");

    let shape: Vec<(String, u16)> = e
        .body()
        .rows
        .iter()
        .map(|r| (r.text.clone(), r.depth))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("here".into(), 0),
            ("one".into(), 0),
            ("two".into(), 1),
            ("three".into(), 1),
        ]
    );
    assert!(
        e.body().rows.iter().all(|r| r.is_task()),
        "and they are tasks"
    );
}

/// Four spaces is one level, not two — the indent scale is whatever the pasted
/// block itself uses, rather than a number this code guessed at.
#[test]
fn a_four_space_indent_pastes_one_level_deep() {
    let mut e = rows_at(&[("here", 0)]);

    koru_editor::clipboard::paste_plain_text(&mut e.editor, &mut e.ws, "one\n    two");

    let depths: Vec<u16> = e.body().rows.iter().map(|r| r.depth).collect();
    assert_eq!(depths, vec![0, 0, 1]);
}

// ------------------------------------------------------------ pills and gaps

/// Type a link through the real prompt: `Ctrl+K`, a caption, Tab, a URL, Enter.
fn commit_a_link(h: &mut Harness, caption: &str, url: &str) {
    h.key(&ch("k"), None, PRIMARY);
    for c in caption.chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    h.key(&named(NamedKey::Tab), None, NONE);
    for c in url.chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    h.key(&named(NamedKey::Enter), None, NONE);
}

// ------------------------------------------------------ prompt fields
//
// An open panel is drawn *over* the rows, so every pointer gesture inside one
// has to be claimed by the panel. What these guard against is not a field
// misbehaving — it is the gesture reaching the invisible row underneath and
// quietly editing the document.

/// The middle of one of the link prompt's boxes, in window coordinates.
fn field_point(h: &Harness, which: link_prompt::Field) -> (f32, f32) {
    let panel = h.editor.overlay_panel_rect().expect("an open panel");
    let f = link_prompt::field_rect(which, panel.w);
    (panel.x + f.x + f.w * 0.5, panel.y + f.y + f.h * 0.5)
}

/// A drag that starts in a field belongs to that field, wherever the pointer
/// goes.
///
/// A guard rather than a regression: before field drags existed a panel press
/// never armed *any* drag, so this passed by doing nothing. It pins that the
/// new path cannot leak into the rows — which is the way it would fail.
#[test]
fn dragging_inside_a_prompt_does_not_select_the_rows_underneath() {
    let mut h = outline();
    h.key(&ch("k"), None, PRIMARY);
    let (x, y) = field_point(&h, link_prompt::Field::Url);

    h.mouse_down(x, y, false);
    h.mouse_move(x + 400.0, y + 300.0);
    h.mouse_up();

    assert!(!h.body().has_row_selection(), "no rows were selected");
    assert!(
        h.body().focused().unwrap().selection.is_collapsed(),
        "and no text in a row either"
    );
}

/// The same for a double click, which never consulted the overlay at all —
/// it word-selected an invisible row, and that became what the next Ctrl+X
/// would have acted on.
#[test]
fn double_clicking_in_a_prompt_does_not_word_select_a_row() {
    let mut h = outline();
    h.key(&ch("k"), None, PRIMARY);
    let (x, y) = field_point(&h, link_prompt::Field::Name);
    let before = h.body().focus;

    h.mouse_double_click(x, y);

    assert_eq!(h.body().focus, before, "the focus did not move to a row");
    assert!(h.body().focused().unwrap().selection.is_collapsed());
}

/// A box that looks typable should look typable.
#[test]
fn hovering_a_prompt_field_shows_the_text_cursor() {
    let mut h = outline();
    h.key(&ch("k"), None, PRIMARY);
    let (x, y) = field_point(&h, link_prompt::Field::Url);

    h.mouse_move(x, y);
    assert_eq!(h.view().cursor_icon(), CursorIcon::Text);

    // The panel's hint text is not a field, so it keeps the arrow.
    let panel = h.editor.overlay_panel_rect().unwrap();
    h.mouse_move(panel.x + 4.0, panel.y + panel.h - 6.0);
    assert_eq!(h.view().cursor_icon(), CursorIcon::Default);
}

/// A field's undo is its own. Reaching the document would move or delete the
/// row the panel is anchored to, and the panel would then draw somewhere else.
#[test]
fn ctrl_z_in_a_prompt_does_not_undo_the_document() {
    let mut h = one_row("a task");
    h.key(&named(NamedKey::End), None, NONE);
    for c in " more".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    assert_eq!(text_of(&h), "a task more");

    h.key(&ch("k"), None, PRIMARY);
    for c in "Terms".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    h.key(&ch("z"), None, PRIMARY);

    let Overlay::Link(prompt) = &h.view().overlay else {
        panic!("the prompt is still open");
    };
    assert!(prompt.name_text().is_empty(), "the field's typing went");
    assert_eq!(text_of(&h), "a task more", "and the row is untouched");
}

/// Every chord an open prompt claims stops there. Ctrl+← is the new one.
#[test]
fn ctrl_left_in_a_prompt_does_not_reach_the_row() {
    let mut h = one_row("one two three");
    h.key(&named(NamedKey::End), None, NONE);
    h.key(&ch("k"), None, PRIMARY);
    let caret = h.body().focused().unwrap().caret();

    h.key(&named(NamedKey::ArrowLeft), None, PRIMARY);

    assert_eq!(
        h.body().focused().unwrap().caret(),
        caret,
        "the row's caret did not move"
    );
}

/// Selecting a URL and pressing `Ctrl+K` fills the URL in and leaves you
/// typing the caption — the half still to be answered.
#[test]
fn ctrl_k_on_a_selected_url_prefills_it_and_focuses_the_name() {
    let mut e = with_selection("see https://www.domain.tld now", 4..26);
    assert!(e.key(&ch("k"), None, PRIMARY));

    let Overlay::Link(prompt) = &e.view().overlay else {
        panic!("Ctrl+K should open the link prompt");
    };
    assert_eq!(prompt.url_text(), "https://www.domain.tld");
    assert!(prompt.name_text().is_empty());
    assert_eq!(prompt.field, link_prompt::Field::Name, "typing the caption");
}

/// And selecting a word fills the caption in, leaving you typing the link.
#[test]
fn ctrl_k_on_a_selected_word_prefills_the_name_and_focuses_the_url() {
    let mut e = with_selection("read the terms today", 9..14);
    assert!(e.key(&ch("k"), None, PRIMARY));

    let Overlay::Link(prompt) = &e.view().overlay else {
        panic!("Ctrl+K should open the link prompt");
    };
    assert_eq!(prompt.name_text(), "terms");
    assert!(prompt.url_text().is_empty());
    assert_eq!(prompt.field, link_prompt::Field::Url, "typing the link");
}

/// The pill takes the selection's place rather than landing beside it.
#[test]
fn committing_a_link_replaces_what_was_selected() {
    let mut e = with_selection("read the terms today", 9..14);
    e.key(&ch("k"), None, PRIMARY);
    // The caption came from the selection; type the URL and commit.
    for c in "example.com".chars() {
        e.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    e.key(&named(NamedKey::Enter), None, NONE);

    assert_eq!(
        text_of(&e),
        "read the \u{2060} today",
        "the word became the pill"
    );
    let attrs = e.ws.read(e.body().rows[0].id).unwrap();
    let attr = attrs.content.attrs().first().expect("a link attribute");
    assert_eq!(attr.label.as_deref(), Some("terms"));
}

/// With nothing selected it still just inserts at the caret.
#[test]
fn ctrl_k_with_no_selection_opens_an_empty_prompt() {
    let mut e = one_row("plain");
    e.key(&named(NamedKey::End), None, NONE);
    assert!(e.key(&ch("k"), None, PRIMARY));

    let Overlay::Link(prompt) = &e.view().overlay else {
        panic!("Ctrl+K should open the link prompt");
    };
    assert!(prompt.name_text().is_empty() && prompt.url_text().is_empty());
    assert_eq!(prompt.field, link_prompt::Field::Name);
}

/// A pill is a word of its own, so committing one leaves a gap after it.
///
/// Without the gap the next thing typed runs into the pill — and a `/` there is
/// not at a word boundary, so the command menu refuses to open in the one place
/// you have just proved you wanted it.
#[test]
fn committing_a_pill_leaves_a_space_after_it() {
    let mut h = one_row("due");
    h.key(&named(NamedKey::End), None, NONE);
    commit_a_link(&mut h, "Terms", "example.com");

    let text = text_of(&h);
    assert!(text.ends_with(' '), "a gap follows the anchor: {text:?}");
    // And the caret is past it, ready to type.
    let caret = h.body().focused().unwrap().caret();
    assert_eq!(caret, text.len());
}

/// Which is what makes the very next `/` work.
#[test]
fn a_slash_straight_after_a_pill_opens_the_command_menu() {
    let mut h = one_row("due");
    h.key(&named(NamedKey::End), None, NONE);
    commit_a_link(&mut h, "Terms", "example.com");

    assert!(h.key(&ch("/"), Some("/"), NONE));

    assert!(
        matches!(h.view().overlay, Overlay::Command(_)),
        "the gap put the caret at a word boundary"
    );
}

/// Re-picking a pill that already has a gap must not stack up a second one.
#[test]
fn re_committing_a_pill_does_not_add_a_second_space() {
    let mut h = one_row("due");
    h.key(&named(NamedKey::End), None, NONE);
    commit_a_link(&mut h, "Terms", "example.com");
    let once = text_of(&h);

    // Re-open its picker from the keyboard, and commit again. Clicking the
    // pill used to do this and no longer can: a plain click on a link now
    // *follows* it. Walking left onto the pill selects it, and Enter opens the
    // same picker through the same `open_pill_picker` — caret placed just past
    // the anchor either way, which is the state this is about.
    h.key(&named(NamedKey::ArrowLeft), None, NONE); // over the gap
    h.key(&named(NamedKey::ArrowLeft), None, NONE); // onto the pill
    assert!(
        h.view().selected_pill.is_some(),
        "walking left off the gap selects the pill"
    );
    h.key(&named(NamedKey::Enter), None, NONE);
    commit_a_link(&mut h, "Terms", "example.com");

    assert_eq!(text_of(&h), once, "still exactly one gap");
}

/// **This suite must not open browser windows**, and the thing that stops it is
/// a feature flag in `Cargo.toml` rather than anything visible here.
///
/// A plain click on a link hands the URL to the OS handler. `cfg(test)` inside
/// `koru-editor` covers that crate's own tests and nothing above it — *a
/// dependency is never compiled with `cfg(test)`* — so `apps/koru` turns on
/// `no-browser` in its `[dev-dependencies]`. Delete that line and every link
/// click in these seven hundred tests becomes a real window; this is what says
/// so. The counterpart of `a_surface_built_under_test_dials_nothing`.
#[test]
fn a_link_click_under_test_opens_nothing() {
    koru_editor::browser::clear_last_launch();
    koru_editor::browser::open("https://example.com").unwrap();
    assert_eq!(
        koru_editor::browser::last_launch(),
        Some(("https://example.com".into(), true)),
        "`no-browser` is off — this suite is launching real browser windows"
    );
}

/// A rename must not insert a pill — it reuses the prompt, not the commit.
#[test]
fn renaming_inserts_nothing_into_the_body() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();
    // Past the title: renaming *does* change row 0, because row 0 is the name.
    // What must not change is the prose under it.
    let body_before: Vec<_> = h.body().rows[BODY..]
        .iter()
        .map(|r| r.text.clone())
        .collect();

    h.editor.start_rename(&h.ws);
    for c in "x".chars() {
        h.key(&ch(&c.to_string()), Some(&c.to_string()), NONE);
    }
    h.key(&named(NamedKey::Enter), None, NONE);

    let body_after: Vec<_> = h.body().rows[BODY..]
        .iter()
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(body_before, body_after, "the body is untouched");
    assert!(
        h.body().rows.iter().all(|r| r.pills.is_empty()),
        "and no pill was inserted"
    );
}

/// An empty name would leave the document unnameable, so it is refused.
#[test]
fn renaming_to_nothing_keeps_the_old_name() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    h.editor.reflow();

    h.editor.start_rename(&h.ws);
    // The old name is selected, so Backspace clears the field.
    h.key(&named(NamedKey::Backspace), None, NONE);
    h.key(&named(NamedKey::Enter), None, NONE);

    assert_eq!(h.ws.read(doc).unwrap().content.as_text(), Some("spec.md"));
}

/// A view with **no root** has nothing to rename, so F2 does nothing rather
/// than opening a prompt over whichever node it happens to be showing.
///
/// Driven on a bare `Editor` because every `Leaf` is rooted now: the task
/// outline was the unrooted view, and it is gone. `Editor::over` is still the
/// unrooted constructor — a second window on a whole document — so the state
/// this guards against is still reachable.
#[test]
fn a_view_with_no_root_has_no_title_to_rename() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    ws.insert(Parent::Root, Node::paragraph("hello world").unwrap())
        .unwrap();
    let mut e = Editor::over(&ws);
    assert!(e.body().root.is_none(), "the premise: nothing is rooted");
    assert!(!e.start_rename(&ws));
    assert!(!e.view().overlay.is_open());
}

// ------------------------------------------------------------ opening

/// **A brand-new workspace opens on nothing at all, and that is the change.**
///
/// It used to open on one empty task: the task outline had to have something
/// typable, so `Leaf::tasks` seeded a line into the `outline/` realm. The
/// outline is gone with the realm ([ADR-90](../../../docs/decisions/README.md)), and a
/// container that shows documents does not want an invisible task node —
/// seeding one is how a workspace grows clutter nothing will ever display.
/// `prepare` is now only "turn on undo".
///
/// Kept as a test rather than deleted with `seed_outline_line`, because the
/// absence is the thing worth pinning: nothing else covers it, since only
/// `Session::open` touches disk and tests never do.
#[test]
fn a_fresh_workspace_opens_on_nothing() {
    let mut workspace = Workspace::new(WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap());
    koru_editor::prepare(&mut workspace);

    assert!(
        workspace.ws.walk().unwrap().is_empty(),
        "opening a workspace writes no node"
    );
    assert!(workspace.session.is_none(), "no disk in tests");
}

/// A view over an empty document has no rows, and adds none: a second window
/// must not write into somebody else's document.
#[test]
fn a_second_view_adds_nothing() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("Doc").unwrap())
        .unwrap();
    let editor = list_block(&ws, doc);
    assert!(editor.body().rows.is_empty());
}

// ------------------------------------------------------------ two views

/// The claim the whole ownership refactor exists to support: two views over
/// one document, each with its own caret and scroll, both seeing the same
/// work. This is the headless half of "type in one window, watch the other
/// follow".
#[test]
fn a_second_view_sees_what_the_first_one_typed() {
    let mut h = one_row("Reward");
    // A second window onto the same document. It seeds nothing — the
    // document is already somebody else's.
    let mut other = Editor::over(&h.ws);
    assert_eq!(other.body().rows.len(), 1);

    h.key(&named(NamedKey::End), None, NONE);
    h.key(&ch("!"), Some("!"), NONE);
    h.key(&named(NamedKey::Enter), None, NONE);
    h.key(&ch("x"), Some("x"), NONE);

    // Until it adopts, the second view is looking at a stale cache — which
    // is fine, because a cache is all it is.
    assert_eq!(other.body().rows.len(), 1, "still stale");
    other.adopt(&h.ws);

    let texts: Vec<_> = other.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(texts, vec!["Reward!".to_string(), "x".to_string()]);
}

// ------------------------------------------------------------ the model

/// The point of the whole port: a view can ask for tasks by attribute
/// without parsing a single character of text.
#[test]
fn pills_make_a_row_queryable_by_assignee_and_date() {
    let mut e = outline();
    let kim = koru_workspace::contact(&mut e.ws, "Kim").unwrap();
    add_pill(
        &mut e,
        3,
        &Attr::new("assignee", AttrValue::Ref(Ref::new(kim))),
    );
    add_pill(
        &mut e,
        4,
        &Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            )),
        ),
    );

    let assigned =
        e.ws.query(&koru_core::TaskQuery::new().assignee(kim))
            .unwrap();
    assert_eq!(assigned.len(), 1);
    assert_eq!(
        assigned[0].assignees.first().map(|a| a.label().to_string()),
        Some("Kim".into()),
        "the card can render the person, not just an id"
    );

    let due =
        e.ws.query(
            &koru_core::TaskQuery::new().due(koru_core::DateRange::between(
                NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            )),
        )
        .unwrap();
    assert_eq!(due.len(), 1, "exactly the row with the August pill");
}

// ------------------------------------------------------------ deleting rows

#[test]
fn ctrl_d_deletes_the_focused_row() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let doomed = e.body().focused_id().unwrap();
    let before = e.body().rows.len();

    assert!(e.key(&ch("d"), None, PRIMARY));
    assert_eq!(e.body().rows.len(), before - 1);
    assert!(
        e.body().rows.iter().all(|r| r.id != doomed),
        "and it is gone from the document, not just the view"
    );
    assert!(
        e.body().focused().is_some(),
        "the caret landed on a real row"
    );
}

/// Deleting a row outdents its children rather than deleting them with it.
#[test]
fn ctrl_d_on_a_parent_promotes_its_children() {
    let mut e = rows_at(&[("Phase", 0), ("Reward", 1), ("Detail", 2), ("Legal", 0)]);
    e.body_mut().focus = 0;

    assert!(e.key(&ch("d"), None, PRIMARY));
    let rows: Vec<(&str, u16)> = e
        .body()
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.depth))
        .collect();
    assert_eq!(
        rows,
        vec![("Reward", 0), ("Detail", 1), ("Legal", 0),],
        "the children outdent into the deleted row's place, keeping their shape"
    );
}

/// A body with no rows is a dead end — nothing to put a caret in, so typing
/// does nothing and there is no way back. Deleting the last row leaves a fresh
/// one instead.
#[test]
fn deleting_the_last_row_leaves_somewhere_to_type() {
    let mut e = one_row("the only task");
    assert!(e.key(&ch("d"), None, PRIMARY));

    assert_eq!(e.body().rows.len(), 1, "a fresh row took its place");
    assert_eq!(e.body().rows[0].text, "");
    assert!(e.body().focused().is_some());

    e.key(&ch("x"), Some("x"), NONE);
    assert_eq!(text_of(&e), "x", "and it can be typed into");
}

/// One Ctrl+Z brings the row back, rather than the delete folding into whatever
/// was typed a moment earlier.
#[test]
fn undo_restores_a_deleted_row_in_one_step() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let text = e.body().focused().unwrap().text.clone();
    let before = e.body().rows.len();

    e.key(&ch("d"), None, PRIMARY);
    assert_eq!(e.body().rows.len(), before - 1);

    e.key(&ch("z"), None, PRIMARY);
    assert_eq!(e.body().rows.len(), before);
    assert!(
        e.body().rows.iter().any(|r| r.text == text),
        "the row is back with its text"
    );
}

/// Ctrl+D is `Scope::Shared`, so a paragraph is as deletable as a task.
#[test]
fn ctrl_d_deletes_a_document_paragraph() {
    let mut h = one_row("a task");
    let doc = koru_workspace::document(&mut h.ws, "spec.md").unwrap();
    as_document(&mut h, doc);
    let first = h.body().rows[BODY].id;
    h.ws.insert_text(first, 0, "Overview").unwrap();
    h.editor.body_mut().refresh(&h.ws, first);
    h.editor.reflow();
    h.ws.insert(
        koru_core::schema::Parent::Node(doc),
        koru_core::node::Node::paragraph("Second").unwrap(),
    )
    .unwrap();
    h.editor.body_mut().rebuild(&h.ws);
    h.editor.reflow();
    let before = h.body().rows.len();

    h.editor.body_mut().focus = BODY;
    assert!(h.key(&ch("d"), None, PRIMARY));
    assert_eq!(h.body().rows.len(), before - 1);
}

/// Undo brings the row back *and* the caret with it. Without this the caret
/// stays on the row that took the deleted one's place, because that row still
/// exists and `rebuild` has no reason to move it.
#[test]
fn undoing_a_delete_puts_the_caret_back_where_it_was() {
    let mut e = outline();
    e.body_mut().focus = 2;
    let doomed = e.body().focused_id().unwrap();
    let text = e.body().focused().unwrap().text.clone();
    assert!(text.len() > 4, "somewhere in the middle to aim at");

    // Put the caret somewhere that is not the start, so landing at zero fails.
    e.editor.place_caret(4);
    e.reflow();
    assert_eq!(e.body().focused().unwrap().caret(), 4);

    e.key(&ch("d"), None, PRIMARY);
    e.key(&ch("z"), None, PRIMARY);

    assert_eq!(
        e.body().focused_id(),
        Some(doomed),
        "the caret went to the restored row, not the one that replaced it"
    );
    assert_eq!(e.body().focused().unwrap().caret(), 4, "and to its offset");
}

/// Undoing an edit must *not* chase anything: nothing came back, so the caret
/// stays where the user left it.
#[test]
fn undoing_a_plain_edit_leaves_the_caret_alone() {
    let mut e = one_row("Reward");
    e.key(&named(NamedKey::End), None, NONE);
    e.key(&ch("!"), Some("!"), NONE);
    let id = e.body().focused_id().unwrap();

    e.key(&ch("z"), None, PRIMARY);
    assert_eq!(text_of(&e), "Reward");
    assert_eq!(e.body().focused_id(), Some(id));
}

// ------------------------------------------------------- selecting rows

fn selected(e: &Harness) -> Vec<String> {
    e.body()
        .rows
        .iter()
        .filter(|r| r.selected)
        .map(|r| r.text.clone())
        .collect()
}

/// Shift+Down runs out of text inside the line and takes the next *row*.
#[test]
fn shift_down_selects_whole_rows() {
    let mut e = outline();
    e.body_mut().focus = 1;
    let first = e.body().rows[1].text.clone();
    let second = e.body().rows[2].text.clone();

    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert_eq!(selected(&e), vec![first.clone(), second.clone()]);

    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert_eq!(selected(&e).len(), 3, "and keeps taking them");
}

/// Reversing shrinks it rather than starting again, which only works because
/// the range is derived from an anchor and the focus.
#[test]
fn shift_up_shrinks_a_downward_selection() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert_eq!(selected(&e).len(), 3);

    e.key(&named(NamedKey::ArrowUp), None, SHIFT);
    assert_eq!(selected(&e).len(), 2, "back down to two");
}

/// A selection belongs to the caret that made it.
#[test]
fn moving_or_typing_drops_the_selection() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert!(!selected(&e).is_empty());

    e.key(&named(NamedKey::ArrowDown), None, NONE);
    assert!(selected(&e).is_empty(), "a plain arrow ends it");

    e.key(&named(NamedKey::ArrowUp), None, SHIFT);
    assert!(!selected(&e).is_empty());
    e.key(&ch("x"), Some("x"), NONE);
    assert!(selected(&e).is_empty(), "and so does typing");
}

/// The rows are thrown away and remade by a rebuild; the anchor is what
/// survives, so the marks come back.
#[test]
fn a_selection_survives_a_rebuild() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let before = selected(&e);

    e.editor.body_mut().rebuild(&e.ws);
    assert_eq!(selected(&e), before);
}

#[test]
fn ctrl_d_deletes_every_selected_row() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let doomed = selected(&e);
    assert_eq!(doomed.len(), 3);
    let before = e.body().rows.len();

    assert!(e.key(&ch("d"), None, PRIMARY));
    assert_eq!(e.body().rows.len(), before - 3);
    for text in doomed {
        assert!(
            e.body().rows.iter().all(|r| r.text != text),
            "{text} should be gone"
        );
    }
}

/// Backspace on a multi-row selection deletes the rows, the same as Ctrl+D,
/// rather than clearing the marks and deleting a character.
#[test]
fn backspace_on_a_row_selection_deletes_the_rows() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let doomed = selected(&e);
    assert_eq!(doomed.len(), 3);
    let before = e.body().rows.len();

    assert!(e.key(&named(NamedKey::Backspace), None, NONE));
    assert_eq!(e.body().rows.len(), before - 3);
    for text in doomed {
        assert!(
            e.body().rows.iter().all(|r| r.text != text),
            "{text} should be gone"
        );
    }
}

/// The Delete key does the same as Backspace on a multi-row selection.
#[test]
fn delete_on_a_row_selection_deletes_the_rows() {
    let mut e = outline();
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let doomed = selected(&e);
    assert_eq!(doomed.len(), 3);
    let before = e.body().rows.len();

    assert!(e.key(&named(NamedKey::Delete), None, NONE));
    assert_eq!(e.body().rows.len(), before - 3);
    for text in doomed {
        assert!(
            e.body().rows.iter().all(|r| r.text != text),
            "{text} should be gone"
        );
    }
}

/// A selection holding a parent *and* its child deletes the child too: an
/// explicitly selected row is deleted, not outdented with the parent's other
/// children.
#[test]
fn deleting_a_parent_and_its_child_deletes_once() {
    let mut e = rows_at(&[("Phase", 0), ("Reward", 1), ("Legal", 0)]);
    e.body_mut().focus = 0;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert_eq!(selected(&e).len(), 2, "the parent and its child");

    assert!(e.key(&ch("d"), None, PRIMARY));
    assert_eq!(
        e.body()
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>(),
        vec!["Legal".to_string()]
    );
}

/// Tab indents the tops of the selection; a child travels with its parent
/// rather than being indented a second time.
#[test]
fn tab_indents_the_tops_of_a_selection() {
    let mut e = rows_at(&[("Anchor", 0), ("One", 0), ("Two", 0), ("Child", 1)]);
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);

    assert!(e.key(&named(NamedKey::Tab), None, NONE));
    let depths: Vec<u16> = e.body().rows.iter().map(|r| r.depth).collect();
    assert_eq!(
        depths,
        vec![0, 1, 1, 2],
        "One and Two went under Anchor; Child kept its place under Two"
    );
}

/// Moving a block up walks top-down, or a row steps over one that has not
/// moved yet.
#[test]
fn ctrl_shift_up_moves_a_selected_block_as_one() {
    let mut e = rows_at(&[("First", 0), ("Second", 0), ("Third", 0)]);
    e.body_mut().focus = 1;
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);

    assert!(e.key(&named(NamedKey::ArrowUp), None, PRIMARY.union(SHIFT)));
    assert_eq!(
        e.body()
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>(),
        vec![
            "Second".to_string(),
            "Third".to_string(),
            "First".to_string()
        ],
        "the pair moved together, keeping their order"
    );
}

// ------------------------------------------- moving a selection out, by @

fn entity_hit(title: &str) -> koru_editor::components::entity_picker::DocumentChoice {
    koru_editor::components::entity_picker::DocumentChoice {
        target: koru_editor::trigger::DocumentTarget {
            document: DocumentId::new(),
            node: None,
        },
        title: title.to_owned(),
        breadcrumb: String::new(),
        is_file: false,
    }
}

fn move_picker_open(e: &Harness) -> bool {
    matches!(&e.view().overlay, Overlay::Entity(p) if p.move_mode)
}

/// `@` over marked rows opens the move picker and inserts nothing; the same
/// keystroke at a caret keeps today's mention trigger.
#[test]
fn at_over_a_row_selection_opens_the_move_picker_and_types_nothing() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert!(e.body().has_row_selection());
    let first = e.body().rows[0].id;
    let before =
        e.ws.read(first)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_owned();

    e.key(&ch("@"), Some("@"), NONE);

    assert!(move_picker_open(&e), "the move picker opened");
    assert_eq!(
        e.ws.read(first).unwrap().content.as_text().unwrap(),
        before,
        "and nothing was typed into the document"
    );

    let mut plain = one_row("x");
    plain.key(&named(NamedKey::End), None, NONE);
    plain.key(&ch(" "), Some(" "), NONE);
    plain.key(&ch("@"), Some("@"), NONE);
    assert!(
        matches!(&plain.view().overlay, Overlay::Entity(p) if !p.move_mode),
        "a caret keeps the ordinary mention picker"
    );
}

/// Choosing a destination changes nothing in the source document; the request
/// carries the selection roots and the edit waits for the shell's completion.
#[test]
fn committing_a_move_destination_leaves_the_source_untouched_until_completion() {
    let mut e = outline();
    // Two sibling children, so both are selection *roots* — a child of a
    // selected row would travel inside its parent's subtree instead.
    e.key(&named(NamedKey::ArrowDown), None, NONE);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let roots = e.body().selection_roots(&e.ws);
    assert_eq!(roots.len(), 2);
    e.key(&ch("@"), Some("@"), NONE);

    let hit = entity_hit("Roadmap");
    let target = hit.target;
    Editing::set_entity_search_results(&mut e.editor, vec![hit], false);
    e.key(&named(NamedKey::Enter), None, NONE);

    let Some((destination, payload)) = e.editor.take_wants_move_selection() else {
        panic!("a destination was chosen");
    };
    match destination {
        koru_editor::view_state::MoveSelectionDestination::Existing { target: t, title } => {
            assert_eq!(t.document, target.document);
            assert_eq!(title, "Roadmap");
        }
        other => panic!("an existing document was chosen: {other:?}"),
    }
    match payload {
        koru_editor::view_state::MoveSelectionPayload::Rows(ids) => assert_eq!(ids, roots),
        other => panic!("a rows payload: {other:?}"),
    }
    for id in &roots {
        assert!(e.ws.read(*id).is_ok(), "nothing was deleted yet");
    }

    // The shell reports the durable destination write; the rows become a pill.
    assert!(
        e.editor
            .complete_selection_move(&mut e.ws, "Roadmap".into(), target)
    );
    for id in &roots {
        assert!(e.ws.read(*id).is_err(), "the moved rows are gone");
    }
    let pill = e
        .body()
        .rows
        .iter()
        .find(|row| !row.pills.is_empty())
        .expect("the trail pill's row");
    let node = e.ws.read(pill.id).unwrap();
    assert_eq!(
        node.content.attrs()[0]
            .as_document_reference()
            .map(|r| r.document),
        Some(target.document)
    );
}

/// Escape abandons the move whole: the picker closes and a later completion
/// (a stale answer from the shell) refuses rather than deleting anything.
#[test]
fn escape_abandons_the_move_and_a_late_completion_refuses() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);
    assert!(move_picker_open(&e));

    e.key(&named(NamedKey::Escape), None, NONE);

    assert!(!e.view().overlay.is_open());
    let target = koru_editor::trigger::DocumentTarget {
        document: DocumentId::new(),
        node: None,
    };
    assert!(
        !e.editor
            .complete_selection_move(&mut e.ws, "Late".into(), target),
        "no pending move survives an Escape"
    );
    assert!(e.body().rows.iter().all(|row| row.pills.is_empty()));
}

/// Typing into the move picker lands in its own field — the document is not a
/// place to type while rows are marked — and "Add new page" falls back to the
/// first selected line when nothing was typed.
#[test]
fn the_move_pickers_query_is_its_own_and_new_page_defaults_to_the_first_line() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let first = e.body().rows[0].id;
    let before =
        e.ws.read(first)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_owned();
    e.key(&ch("@"), Some("@"), NONE);

    e.key(&ch("n"), Some("n"), NONE);
    assert_eq!(
        e.ws.read(first).unwrap().content.as_text().unwrap(),
        before,
        "the keystroke landed in the picker's field, not the document"
    );

    // Backspace the query away and commit the pinned "Add new page" row.
    e.key(&named(NamedKey::Backspace), None, NONE);
    e.key(&named(NamedKey::Enter), None, NONE);

    let Some((destination, _)) = e.editor.take_wants_move_selection() else {
        panic!("the new-page destination was chosen");
    };
    match destination {
        koru_editor::view_state::MoveSelectionDestination::NewPage {
            title,
            follow,
            child,
        } => {
            assert_eq!(title, before, "titled from the first selected line");
            assert!(follow, "a plain Enter opens the new document");
            assert!(!child, "and creates it as a sibling");
        }
        other => panic!("a new page: {other:?}"),
    }
}

/// An inline move: the selected text leaves as runs and the pill takes exactly
/// the selection's place.
#[test]
fn moving_a_text_selection_replaces_it_with_the_pill() {
    let mut e = with_selection("payout flow", 0..6);
    assert!(e.editor.open_move_selection_picker(&e.ws));
    assert!(move_picker_open(&e));

    let hit = entity_hit("Billing");
    let target = hit.target;
    Editing::set_entity_search_results(&mut e.editor, vec![hit], false);
    e.key(&named(NamedKey::Enter), None, NONE);

    let Some((_, payload)) = e.editor.take_wants_move_selection() else {
        panic!("a destination was chosen");
    };
    match &payload {
        koru_editor::view_state::MoveSelectionPayload::Inline { runs, .. } => {
            let text: String = runs.iter().map(|run| run.text.as_str()).collect();
            assert_eq!(text, "payout");
        }
        other => panic!("an inline payload: {other:?}"),
    }

    assert!(
        e.editor
            .complete_selection_move(&mut e.ws, "Billing".into(), target)
    );
    let row = e
        .body()
        .rows
        .iter()
        .find(|row| !row.pills.is_empty())
        .unwrap();
    let text =
        e.ws.read(row.id)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_owned();
    assert_eq!(text, "\u{2060} flow", "the pill stands where the text was");
}

/// The whole list, marks and all: selecting every item of a list and moving it
/// must still complete — this is the report from first field use, where the
/// completion refused and the content was left duplicated.
#[test]
fn moving_every_item_of_a_list_completes() {
    let mut e = rows_at(&[("one", 0), ("two", 0), ("three", 0)]);
    let first = e.body().rows[0].id;
    e.ws.set_mark(first, 0..3, Mark::Bold, true).unwrap();
    e.editor.body_mut().refresh(&e.ws, first);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    let roots = e.body().selection_roots(&e.ws);
    assert_eq!(roots.len(), 3, "every item is a root");
    e.key(&ch("@"), Some("@"), NONE);
    let hit = entity_hit("Elsewhere");
    let target = hit.target;
    Editing::set_entity_search_results(&mut e.editor, vec![hit], false);
    e.key(&named(NamedKey::Enter), None, NONE);
    assert!(e.editor.take_wants_move_selection().is_some());

    assert!(
        e.editor
            .complete_selection_move(&mut e.ws, "Elsewhere".into(), target),
        "the completion must not refuse: {:?}",
        e.view().notice
    );
    assert_eq!(
        e.body()
            .rows
            .iter()
            .filter(|row| !row.pills.is_empty())
            .count(),
        1,
        "one trail pill remains"
    );
}

/// A text selection that escalates into a block selection and shrinks back is
/// the text selection again — the starting range survives the round trip
/// instead of collapsing into a one-row chip.
#[test]
fn shrinking_a_block_selection_back_restores_the_starting_text_range() {
    let mut e = rows_at(&[
        ("App Intents and Shortcuts", 0),
        ("Open Document", 0),
        ("Open Profile Home", 0),
    ]);
    e.editor.place_selection_pair((0, 11)); // "App Intents"
    e.reflow();
    assert_eq!(e.body().rows[0].range(), 0..11);

    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    assert!(e.body().has_row_selection(), "escalated to blocks");

    e.key(&named(NamedKey::ArrowUp), None, SHIFT);
    e.key(&named(NamedKey::ArrowUp), None, SHIFT);

    assert!(
        !e.body().has_row_selection(),
        "back on the starting row, out of block mode"
    );
    assert_eq!(
        e.body().rows[0].range(),
        0..11,
        "the text selection it started as is back"
    );
}

/// A block selection that did *not* start as text keeps today's one-row chip
/// when it shrinks — only a stored range earns the transition back.
#[test]
fn shrinking_without_a_starting_range_keeps_the_row_chip() {
    let mut e = rows_at(&[("one", 0), ("two", 0)]);
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&named(NamedKey::ArrowUp), None, SHIFT);
    assert!(
        e.body().has_row_selection(),
        "a caret-started selection shrinks to a single-row chip, as before"
    );
}

/// The move picker opens holding the **first selected block's** text — a line
/// somebody wrote as a line — truncated to thirty characters. (A prefill from
/// the partial starting selection was tried and read as a torn-off fragment.)
#[test]
fn the_move_picker_prefills_from_the_first_block() {
    let mut e = rows_at(&[
        ("App Intents and Shortcuts", 0),
        ("Open Document", 0),
        ("Open Profile Home", 0),
    ]);
    e.editor.place_selection_pair((0, 11));
    e.reflow();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);

    let Overlay::Entity(picker) = &e.view().overlay else {
        panic!("the move picker is open");
    };
    assert!(picker.move_mode);
    assert_eq!(
        picker.own_query.as_ref().map(|f| f.raw_text()),
        Some("App Intents and Shortcuts"),
        "the whole first block, not the partial selection"
    );
    assert_eq!(
        picker.query, "App Intents and Shortcuts",
        "and the search is driven by it"
    );

    // A long first block is cut at thirty characters.
    let mut long = rows_at(&[
        (
            "global should mean global within the current profile, not across all",
            0,
        ),
        ("Open Document", 0),
    ]);
    long.key(&named(NamedKey::ArrowDown), None, SHIFT);
    long.key(&ch("@"), Some("@"), NONE);
    let Overlay::Entity(picker) = &long.view().overlay else {
        panic!("the move picker is open");
    };
    assert_eq!(
        picker.own_query.as_ref().map(|f| f.raw_text()),
        Some("global should mean global…"),
        "cut at the last word inside thirty characters, and says so"
    );
}

/// Ctrl+Enter commits the move but stays put: the destination is created and
/// filled, the pill lands, and no navigation follows.
#[test]
fn ctrl_enter_moves_without_following() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);

    e.key(&named(NamedKey::Enter), None, PRIMARY);

    let Some((destination, _)) = e.editor.take_wants_move_selection() else {
        panic!("the commit still happened");
    };
    match destination {
        koru_editor::view_state::MoveSelectionDestination::NewPage { follow, .. } => {
            assert!(!follow, "Ctrl+Enter means stay");
        }
        other => panic!("a new page: {other:?}"),
    }
}

/// A destination with no title still gets a readable trail pill — never an
/// empty chip standing where a paragraph was.
#[test]
fn an_untitled_destination_still_labels_the_trail_pill() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);
    let hit = entity_hit("");
    Editing::set_entity_search_results(&mut e.editor, vec![hit], false);
    e.key(&named(NamedKey::Enter), None, NONE);

    let Some((destination, _)) = e.editor.take_wants_move_selection() else {
        panic!("a destination was chosen");
    };
    match destination {
        koru_editor::view_state::MoveSelectionDestination::Existing { title, .. } => {
            assert_eq!(title, "Untitled");
        }
        other => panic!("an existing document: {other:?}"),
    }
}

/// Shift+Enter makes the created document a child of the one the selection
/// left, and stays there — a sub-page you mean to keep working past.
#[test]
fn shift_enter_creates_a_child_and_stays() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);

    e.key(&named(NamedKey::Enter), None, SHIFT);

    let Some((destination, _)) = e.editor.take_wants_move_selection() else {
        panic!("the commit still happened");
    };
    match destination {
        koru_editor::view_state::MoveSelectionDestination::NewPage { follow, child, .. } => {
            assert!(child, "Shift+Enter creates a child");
            assert!(!follow, "and stays put");
        }
        other => panic!("a new page: {other:?}"),
    }
}

/// Feeding the same recents twice reports no change — the feed runs on every
/// gesture sync, and an unconditional "changed" was a redraw loop pinned at
/// full speed, felt as "@ blocks the UI".
#[test]
fn refeeding_identical_recents_is_not_a_change() {
    let mut e = outline();
    e.key(&named(NamedKey::ArrowDown), None, SHIFT);
    e.key(&ch("@"), Some("@"), NONE);
    // Clear the prefill so the picker is in its empty-query recents state.
    while {
        let Overlay::Entity(p) = &e.view().overlay else {
            panic!("move picker open")
        };
        !p.own_query.as_ref().is_some_and(|f| f.is_empty())
    } {
        e.key(&named(NamedKey::Backspace), None, NONE);
    }
    assert!(Editing::entity_wants_recents(&e.editor));

    let hits = vec![entity_hit("Alpha"), entity_hit("Beta")];
    assert!(Editing::set_entity_recents(&mut e.editor, hits.clone()));
    assert!(
        !Editing::set_entity_recents(&mut e.editor, hits),
        "an identical refeed is quiet"
    );
}

#[test]
fn ordinary_document_picker_accepts_window_recents() {
    let mut e = outline();
    e.key(&ch("@"), Some("@"), NONE);
    assert!(Editing::entity_wants_recents(&e.editor));
    let hit = entity_hit("Recent document");
    assert!(Editing::set_entity_recents(
        &mut e.editor,
        vec![hit.clone()]
    ));
    let Overlay::Entity(picker) = &e.view().overlay else {
        panic!("document picker open");
    };
    assert_eq!(picker.hits, vec![hit]);
}
