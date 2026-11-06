// SPDX-License-Identifier: MPL-2.0

//! **What the Tasks page offers a screen reader, and what a request to one of
//! its controls means.**
//!
//! One sentence, and what it excludes: *this module turns the page's already
//! prepared rows into semantic nodes, and turns a request back into the same
//! call the pointer or the keyboard makes.* It excludes what a row **is** — the
//! query, the buckets, the chips and the pickers are [`super::view`]'s and stay
//! there — and it excludes composition: where the page sits inside a window and
//! what is on top of it belong to the shell (`mainview/accessibility`). A child
//! module rather than a section of `view.rs` for the reason §7.10 gives: that
//! file is 3,000 lines and gets a call, not another embedded subsystem.
//!
//! ## The shape of it
//!
//! ```text
//! Group     the page                s:tasks#page      label = the queue's or the list's name
//! ├── Label     the count           s:tasks#count     value = "12 open"
//! ├── TextInput the capture row     s:tasks#capture   label = "Add a task"
//! ├── ListBox   the rows            s:tasks#list      size_of_set = how many rows there are
//! │   └── ListBoxOption  a task     n:<node uuid>#task-row    position_in_set = its index
//! │       ├── CheckBox   the tick   n:<node uuid>#task-check  toggled = the canonical lifecycle
//! │       ├── Label      residue    n:<node uuid>#task-state  only where SPEC §5's applies
//! │       ├── Link       the list   n:<node uuid>#task-list   only where the chip leads somewhere
//! │       └── Button     the `…`    n:<node uuid>#task-actions
//! └── Group     an open panel       tasks-panel:<n>#root      named, modal, and empty
//! ```
//!
//! ## Four rules it is built on
//!
//! **The middle of three identically titled rows is reachable, and audible.**
//! Identity never comes from a label — a row *is* its task's node, so a client
//! finds the second of three by `AutomationId` whatever the three are called.
//! What makes them tellable apart **by ear** is the pair AccessKit actually
//! carries: [`position_in_set`](koru_workspace_host::accessibility::SemanticNode::position)
//! on the row, stored zero-based because `accesskit_windows` adds the one a
//! reader hears, and `size_of_set` on the **container** — which is why the list
//! is a [`Role::ListBox`] and not a `Role::List`. `accesskit_consumer` answers
//! an item's total by walking up to a parent whose role it counts as a
//! container of selectable children, and `List` is not one of them, so the
//! total would vanish silently. See `guide/contributing/deps/ACCESSKIT.md`.
//!
//! **The box says *completed*, not *done*.** Every row on this page is a task
//! and the tick it draws is the canonical lifecycle
//! ([`TaskEntry::completed`](super::query::TaskEntry::completed)) — unlike a
//! document's box, which is the `done` mirror and is allowed to disagree with
//! it (`docs/features/tasks/SPEC.md` §5). So the two surfaces use two words, and the one
//! thing this page's box cannot say — completed while still holding unfinished
//! work — gets a node of its own, exactly where the warning triangle is drawn.
//!
//! **Geometry is never re-derived.** Every rectangle comes back from the
//! function drawing and hit-testing already share —
//! [`table`](super::view::TasksView::table) with
//! [`bands`](super::view::TasksView::bands),
//! [`parts`](super::view::TasksView::parts),
//! [`row_chip_rects`](super::view::TasksView::row_chip_rects),
//! [`capture_rect`](super::view::TasksView::capture_rect) — and is shifted into
//! the window's space by the one translation in [`Context::place`], because a
//! tab reports in its own coordinates.
//!
//! **An action is the press or the keystroke that already exists.** Every arm
//! of [`TasksView::accessibility_action`] reaches a method the pointer or an
//! arrow key reaches, and each of them leaves as a
//! [`Request`](super::view::Request) the container turns into the ordinary task
//! command. There is no second road from this page to the store.
//!
//! ## What this plan deliberately does not publish
//!
//! The chips row, the header `…` and the *contents* of an open panel. All three
//! draw **English literals** — SPEC §11 made this release's Tasks copy
//! English-only by owner decision — and §7.2 requires an accessible name to go
//! through `koru-i18n` in all eight languages from its first implementation, so
//! naming them would mean authoring a dozen strings this plan was not scoped to
//! translate. None of them is a task operation, and none has a keyboard route
//! either, so nothing here is *taken away* from a screen reader that a keyboard
//! had. An open panel is still published — named, modal and empty, the answer
//! `mainview/accessibility/menus.rs` already gives for a surface whose form
//! belongs to another plan — so that "the keyboard is in a panel" is something a
//! reader can hear instead of a trap. Recorded in
//! `docs/features/accessibility/README.md`.

use accesskit::{Action, Role};
use koru_core::NodeId;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, fill, number, t};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, SemanticNode, ViewSlot, WindowSlot,
};

use super::pickers::Picker;
use super::query::TaskEntry;
use super::view::{Line, TasksView};
use crate::panes::explore::rail::{Bucket, RailTarget};

/// The page's own furniture, as one object. A locator then reads
/// `.../s:tasks#list` and a person can see at a glance which surface a control
/// came from.
const PAGE: &str = "tasks";

/// **One opening of one of this page's floating panels.** See
/// [`TasksView::panel_instance`](super::view::TasksView): an instance number,
/// so a request queued against a panel that has closed cannot land in its
/// successor.
const PANEL: &str = "tasks-panel";

/// The parts of the page that stand for no task.
///
/// Keywords, chosen by the code. A part is never a label and never an index —
/// §5's *reordering, renaming, translating labels or scrolling must not change
/// the identity of a surviving control*.
pub(crate) mod parts {
    use koru_workspace_host::accessibility::ControlPart;

    /// The page itself, and the two things above the list.
    pub const PAGE: ControlPart = ControlPart::new("page");
    pub const COUNT: ControlPart = ControlPart::new("count");
    pub const CAPTURE: ControlPart = ControlPart::new("capture");
    /// The list of rows — the container an item's total is read from.
    pub const LIST: ControlPart = ControlPart::new("list");
    /// The note above the rows, when the page has one.
    pub const NOTE: ControlPart = ControlPart::new("note");

    /// The four parts of one row. Prefixed, because a task's node may also be
    /// a paragraph of the open document (`#text`, `#check`, `#state`) and an
    /// explore row (`#root`), and all of them are scoped by the same uuid.
    pub const ROW: ControlPart = ControlPart::new("task-row");
    pub const CHECK: ControlPart = ControlPart::new("task-check");
    pub const STATE: ControlPart = ControlPart::new("task-state");
    pub const LIST_CHIP: ControlPart = ControlPart::new("task-list");
    pub const ACTIONS: ControlPart = ControlPart::new("task-actions");
}

/// Everything the page needs from the window it is drawn in, which is nothing
/// it could work out for itself.
///
/// `area` is the content region **in window-logical coordinates** — the rect
/// the container hit-tests this tab against. The page lays out at its own
/// origin, so every rectangle it hands back is shifted by exactly this once,
/// in [`place`](Self::place), and nowhere else.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Context {
    pub window: WindowSlot,
    pub view: ViewSlot,
    pub area: Rect,
    pub lang: Lang,
}

impl Context {
    fn surface(&self, part: ControlPart) -> Occurrence {
        Occurrence::surface(self.window, self.view, PAGE, part)
    }

    fn model(&self, id: NodeId, part: ControlPart) -> Occurrence {
        Occurrence::model(self.window, self.view, id.as_uuid(), part)
    }

    fn panel(&self, instance: u64) -> Occurrence {
        Occurrence::instance(self.window, self.view, PANEL, instance, ControlPart::ROOT)
    }

    /// **The one translation between the page's space and the window's.**
    fn place(&self, r: Rect) -> Rect {
        Rect::new(r.x + self.area.x, r.y + self.area.y, r.w, r.h)
    }
}

/// The queue's name, localized. A list says its own name, which is the
/// person's content and is never translated.
///
/// The drawn heading is still English (`rail::bucket_label`) — existing debt
/// this plan is not authorised to respell — so the two can differ in a
/// non-English session, exactly as the explore column's empty state does.
fn page_name(view: &TasksView, lang: Lang) -> String {
    match view.target() {
        RailTarget::Bucket(bucket) => t(
            lang,
            match bucket {
                Bucket::Today => Key::A11yTaskBucketToday,
                Bucket::Next => Key::A11yTaskBucketNext,
                Bucket::Upcoming => Key::A11yTaskBucketUpcoming,
                Bucket::Later => Key::A11yTaskBucketLater,
                Bucket::Completed => Key::A11yTaskBucketCompleted,
            },
        )
        .to_owned(),
        // A list's own name, and the neutral word for an id the table has
        // forgotten — `TasksView::title`'s two arms, said the same way.
        RailTarget::List(_) => view.title(),
        // The same unreachable every Tasks page match carries: the rail's
        // Reminders row swaps the tab's view rather than retargeting this one.
        RailTarget::Reminders => unreachable!(
            "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
        ),
    }
}

impl TasksView {
    /// The page, as a screen reader sees it right now.
    ///
    /// `&self` throughout: nothing here lays out, scrolls, measures or reads
    /// the store, and the signature is what makes that true rather than a
    /// comment.
    /// The note's region — named, so a reader knows the page has one and
    /// where it is. Its rows are the document's to describe (P3).
    fn note_semantics(&self, cx: &Context) -> Option<SemanticNode> {
        let body = self.body.as_ref()?;
        let band = body.band;
        Some(
            SemanticNode::new(cx.surface(parts::NOTE), Role::Group)
                .with_label(t(cx.lang, Key::PageNote).to_owned())
                .with_bounds(Rect::new(
                    cx.area.x + band.x,
                    cx.area.y + band.y,
                    band.w,
                    band.h,
                )),
        )
    }

    pub(crate) fn semantics(&self, cx: &Context) -> SemanticNode {
        let name = page_name(self, cx.lang);
        let mut children = Vec::new();
        children.extend(self.count_semantics(cx));
        children.extend(self.note_semantics(cx));
        children.push(self.capture_semantics(cx));
        children.push(self.list_semantics(cx, &name));
        children.extend(self.panel_semantics(cx));
        SemanticNode::new(cx.surface(parts::PAGE), Role::Group)
            .with_label(name)
            .with_bounds(cx.area)
            .with_children(children)
    }

    /// **Everything this page's tree is built from**, folded into the shell's
    /// revision.
    ///
    /// What is deliberately *absent* is everything that moves without changing
    /// what is published: [`scroll`](Self::scroll) — a scrolled row keeps its
    /// identity, its label and its place in the set — the flash deadline, the
    /// hover, and every chip width. Scrolling moves *rectangles*, and a screen
    /// reader asks for one when it wants it; rebuilding the tree for it is the
    /// per-frame cost §7.3 forbids.
    pub(crate) fn revise(&self, rev: &mut Revision) {
        rev.str(match self.target() {
            RailTarget::Bucket(bucket) => bucket.as_str(),
            RailTarget::List(_) => "list",
            RailTarget::Reminders => unreachable!(
                "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
            ),
        });
        if let RailTarget::List(id) = self.target() {
            rev.uuid(id.as_uuid());
        }
        rev.bool(self.panel.known());
        rev.u64(self.panel.open_count() as u64);
        rev.u64(self.rows.len() as u64);
        for row in &self.rows {
            rev.uuid(row.node.as_uuid());
            rev.str(&row.title);
            rev.str(&row.breadcrumb);
            rev.bool(row.completed);
            rev.bool(row.residue);
            rev.opt_uuid(self.leads_to_list(row).map(|id| id.as_uuid()));
        }
        rev.opt_uuid(self.selected.map(|(_, node)| node.as_uuid()));
        rev.bool(self.capture.is_some());
        // Which panel is up, and *which opening of it* — so a panel that closed
        // and reopened is a different tree rather than the same one.
        rev.byte(match (&self.picker, &self.overflow) {
            (Some(Picker::List(_)), _) => 1,
            (Some(Picker::Priority(_)), _) => 2,
            (Some(Picker::Due(_)), _) => 3,
            (Some(Picker::Color(_)), _) => 5,
            (None, Some(_)) => 4,
            (None, None) => 0,
        });
        rev.u64(self.panel_instance);
        // The page's own size: every published rectangle is relative to it.
        rev.f32(self.viewport.0).f32(self.viewport.1);
    }

    /// **Which control has the keyboard**, read from the states the page is
    /// already drawn from.
    ///
    /// The order is the page's own — a panel takes the arrows, then the capture
    /// row, then the selected row — and it is deliberately not a second router:
    /// every arm asks a field `key` and `press` already read.
    pub(crate) fn accessible_focus(&self, cx: &Context) -> Occurrence {
        if self.picker.is_some() || self.overflow.is_some() {
            return cx.panel(self.panel_instance);
        }
        if self.capture.is_some() {
            return cx.surface(parts::CAPTURE);
        }
        match self.selected_row().and_then(|i| self.rows.get(i)) {
            Some(row) => cx.model(row.node, parts::ROW),
            None => cx.surface(parts::PAGE),
        }
    }

    // ------------------------------------------------------- above the list

    /// How much work this page is showing, as a sentence.
    ///
    /// `None` before the first answer — a `0` meaning *we have not looked yet*
    /// is a lie a reader cannot see through, which is
    /// [`count_label`](Self::count_label)'s own rule kept here. Completed says
    /// *completed*, because *12 open* on an archive is a contradiction.
    fn count_semantics(&self, cx: &Context) -> Option<SemanticNode> {
        if !self.panel.known() {
            return None;
        }
        let (key, n) = match self.target() {
            RailTarget::Bucket(Bucket::Completed) => {
                (Key::A11yTaskCompletedCount, self.rows.len() as u64)
            }
            _ => (Key::A11yTaskOpenCount, self.panel.open_count() as u64),
        };
        let sentence = fill(t(cx.lang, key), &[&number::decimal(cx.lang, n)]);
        let body = self.table().body();
        // A `Role::Label`'s accessible name is read out of its *value*, which
        // `projection::build` fills in from the label for every caller
        // (`guide/contributing/deps/ACCESSKIT.md`). Nothing to do here but say it once.
        Some(
            SemanticNode::new(cx.surface(parts::COUNT), Role::Label)
                .with_label(sentence)
                // The band above the rows, from the table's own two numbers —
                // there is no third copy of the header's arithmetic here.
                .with_bounds(cx.place(Rect::new(0.0, 0.0, self.viewport.0, body.y.max(0.0)))),
        )
    }

    /// The *Add a task…* line — the list's own first row, and the one creation
    /// affordance on this page.
    ///
    /// A `Role::TextInput` in both states, because that is what it is: a field
    /// you put the caret in. **No `value`**, and that is deliberate — a value
    /// would give it UIA's `ValuePattern`, which also offers `SetValue`, and
    /// nothing here performs one; the paragraph projection refuses the same
    /// trade for the same reason. So what is typed into it is not readable
    /// back through the tree yet, which is recorded in `ACCEPTANCE.md` rather
    /// than papered over with a control that lies about what it accepts.
    fn capture_semantics(&self, cx: &Context) -> SemanticNode {
        SemanticNode::new(cx.surface(parts::CAPTURE), Role::TextInput)
            .with_label(t(cx.lang, Key::A11yTaskAdd))
            .with_bounds(cx.place(self.capture_rect()))
            .with_actions([Action::Focus, Action::Click])
    }

    // ------------------------------------------------------------- the list

    /// The rows — **every one of them, not the visible ones.**
    ///
    /// The table culls to the band because that is what drawing wants; a screen
    /// reader wants the opposite. A row below the fold is published at the
    /// rectangle it *would* occupy and [`Action::ScrollIntoView`] on it reveals
    /// it through the page's own [`reveal_row`](Self::reveal_row) — the
    /// roadmap's *"a navigation request can reveal the target using existing
    /// scroll behaviour"*. Nothing here sets `clips_children`, so the consumer
    /// keeps them all in the control view.
    fn list_semantics(&self, cx: &Context, name: &str) -> SemanticNode {
        // **The table and the band list once, not once per row.** Both are
        // O(lines) to build, and asking for them inside the loop would make
        // publishing a page O(rows²) — on a walk that runs whenever anything on
        // this page moves, and only while a client is attached, which is
        // exactly where §7.3's *reuse layout and state* bites.
        let table = self.table();
        let bands = self.bands();
        let children = self
            .lines
            .iter()
            .enumerate()
            .filter_map(|(line, entry)| match entry {
                Line::Task(i) => self.row_semantics(cx, *i, table.row_rect(&bands, line)),
                // A week rule is a label for what is under it, and the capture
                // line is published above the list rather than in it.
                Line::Rule(_) | Line::Capture => None,
            })
            .collect();
        SemanticNode::new(cx.surface(parts::LIST), Role::ListBox)
            .with_label(name.to_owned())
            .with_bounds(cx.place(table.body()))
            // **On the container, because that is where an item's total is read
            // from** — and a `Role::ListBox` because it is one of the roles
            // AccessKit counts as one. See the module note.
            .with_size_of_set(self.rows.len())
            .with_children(children)
    }

    /// One task row, and the three controls inside it. `rect` is the line the
    /// table put it on, so nothing here re-derives a row's place.
    fn row_semantics(&self, cx: &Context, i: usize, rect: Rect) -> Option<SemanticNode> {
        let row = self.rows.get(i)?;
        let parts_of = self.parts(rect);
        let mut children = vec![
            SemanticNode::new(cx.model(row.node, parts::CHECK), Role::CheckBox)
                .with_label(t(cx.lang, Key::A11yTaskDone))
                .with_bounds(cx.place(parts_of.control))
                // **The lifecycle, through a checkbox.** On this page the tick
                // *is* `koru.task_state`; a document's box is the `done` mirror
                // and says *Done* instead. `Toggle()` comes back as
                // `Action::Click`, which is the press on that square.
                .with_toggled(row.completed),
        ];
        children.extend(self.residue_semantics(cx, row, rect));
        children.extend(self.list_chip_semantics(cx, row, rect, i));
        children.push(
            SemanticNode::new(cx.model(row.node, parts::ACTIONS), Role::Button)
                .with_label(t(cx.lang, Key::A11yMoreActions))
                .with_bounds(cx.place(parts_of.dots))
                .with_action(Action::Click),
        );
        Some(
            SemanticNode::new(cx.model(row.node, parts::ROW), Role::ListBoxOption)
                .with_label(row_name(row))
                .with_bounds(cx.place(rect))
                // Zero-based: the adapter adds the one a reader hears.
                .with_position(i)
                // **No `selected`.** Which row the keyboard is on is *focus*,
                // and it is published as focus; carrying `selected` as well
                // would cost every row its Invoke pattern — `is_invocable`
                // excludes a node whose `is_selected()` is `Some` — and leave a
                // client unable to open any of them.
                .with_actions([Action::Click, Action::Focus, Action::ScrollIntoView])
                .with_children(children),
        )
    }

    /// **Completed, and still holding unfinished structural work** — SPEC §5's
    /// concurrent residue, and the warning triangle drawn after the title.
    ///
    /// The one thing this page's tick cannot say. It gets a node only where the
    /// triangle is drawn, because everywhere else the box has already said it:
    /// a document publishes the lifecycle on every task, since there the box is
    /// a different fact.
    fn residue_semantics(&self, cx: &Context, row: &TaskEntry, rect: Rect) -> Option<SemanticNode> {
        row.residue.then(|| {
            SemanticNode::new(cx.model(row.node, parts::STATE), Role::Label)
                .with_label(t(cx.lang, Key::A11yTaskStateResidue))
                .with_bounds(cx.place(rect))
        })
    }

    /// **The list chip, where it leads somewhere.** The one chip that is a
    /// control (the others are labels and stay labels), so it is the one that
    /// is published as one — a `Role::Link`, because following it opens
    /// another page.
    ///
    /// Its name is the list's own, which is the person's content: never
    /// translated, never invented.
    fn list_chip_semantics(
        &self,
        cx: &Context,
        row: &TaskEntry,
        rect: Rect,
        i: usize,
    ) -> Option<SemanticNode> {
        let list = row.list?;
        let face = self.list_face(list)?;
        let chips = self.row_chips(row);
        let at = chips.iter().position(|chip| chip.list.is_some())?;
        let bounds = self.row_chip_rects(rect, i).get(at).copied()?;
        Some(
            // A button now, not a link: it opens the list picker.
            SemanticNode::new(cx.model(row.node, parts::LIST_CHIP), Role::Button)
                .with_label(face.name.clone())
                .with_bounds(cx.place(bounds))
                .with_action(Action::Click),
        )
    }

    // ------------------------------------------------------------ the panel

    /// The overflow menu or one of the three pickers, while one is up.
    ///
    /// **Named, modal, and empty** — `mainview/accessibility/menus.rs`'s own
    /// answer for a surface whose form belongs to another plan. What it buys is
    /// the thing a reader most needs from it: that the keyboard has been taken
    /// by something, and by what. Its rows are English drawn copy and naming
    /// them is a localization change this plan is not scoped to make; see the
    /// module note.
    fn panel_semantics(&self, cx: &Context) -> Option<SemanticNode> {
        let (key, bounds) = match (&self.picker, &self.overflow) {
            (Some(Picker::List(picker)), _) => {
                (Key::A11yTaskListPicker, picker.menu.panel(self.viewport))
            }
            (Some(Picker::Priority(picker)), _) => (
                Key::A11yTaskPriorityPicker,
                picker.menu.panel(self.viewport),
            ),
            (Some(Picker::Due(picker)), _) => (Key::A11yTaskDuePicker, picker.panel),
            (Some(Picker::Color(picker)), _) => {
                (Key::A11yTaskColorPicker, picker.menu.panel(self.viewport))
            }
            (None, Some(menu)) => (Key::A11yMoreActions, menu.panel(self.viewport)),
            (None, None) => return None,
        };
        Some(
            SemanticNode::new(cx.panel(self.panel_instance), Role::Group)
                .with_label(t(cx.lang, key))
                .with_bounds(cx.place(bounds))
                .modal(),
        )
    }

    // ----------------------------------------------------------- the actions

    /// **Do what a client asked, through the call the page already makes.**
    ///
    /// `false` means *this page does not perform that action on that control*,
    /// which is the honest answer to the `Focus` Windows sends ahead of every
    /// invoke on something that is not a row.
    ///
    /// Every arm leaves a [`Request`](super::view::Request) behind, exactly as
    /// the press does; the container drains it in `drain_tasks_request` and
    /// turns it into the one canonical task command. There is no second road.
    pub(crate) fn accessibility_action(&mut self, target: Occurrence, action: Action) -> bool {
        // **A panel owns everything beneath it**, the same answer the keyboard
        // gives: while one is up, `key` never reaches the list at all.
        if self.picker.is_some() || self.overflow.is_some() {
            return false;
        }
        if target.object == ObjectKey::Surface(PAGE)
            && target.part == parts::CAPTURE
            && matches!(action, Action::Focus | Action::Click)
        {
            // The press's own call. The caret lands in the row that is already
            // the list's first line.
            self.begin_capture();
            return true;
        }
        let ObjectKey::Model(uuid) = target.object else {
            return false;
        };
        let node = NodeId::from_uuid(uuid);
        // **The row this request is about, resolved against what is on screen
        // now.** A row that has left the page answers nothing, which is the
        // stale request arriving after the list moved under it.
        let Some(i) = self.rows.iter().position(|row| row.node == node) else {
            return false;
        };
        match (target.part, action) {
            // The press sets the keyboard's row; the invoke opens it. Matching
            // the pointer exactly, which matters because Windows sends `Focus`
            // immediately before `Invoke`.
            (p, Action::Focus) if p == parts::ROW => {
                self.select_row(i);
                self.reveal_row(i);
                true
            }
            (p, Action::Click) if p == parts::ROW => {
                self.select_row(i);
                // **Here, not in a new tab** — `Enter`'s disposition, so a
                // client and the keyboard cannot open two different windows.
                self.open_selected(false)
            }
            (p, Action::ScrollIntoView) if p == parts::ROW => {
                self.reveal_row(i);
                true
            }
            // `Toggle()` arrives as a click, which is what the tick answers.
            (p, Action::Click) if p == parts::CHECK => self.toggle_complete(i),
            (p, Action::Click) if p == parts::ACTIONS => {
                self.select_row(i);
                self.menu_for_selected()
            }
            (p, Action::Click) if p == parts::LIST_CHIP => self.press_list_chip(i, None),
            _ => false,
        }
    }
}

/// **What a reader hears for one row**, which is what is drawn on it: the
/// task's title, and — where it has one — the document it lives in, after the
/// same separator `row::paint` puts there.
///
/// Both halves are the person's content and neither is translated. Neither is
/// an identity: two rows may read identically, and telling them apart is the
/// container's `position_in_set` and the locator's uuid, never this string.
fn row_name(row: &TaskEntry) -> String {
    if row.breadcrumb.is_empty() {
        return row.title.clone();
    }
    format!("{}{}{}", row.title, super::row::DOT, row.breadcrumb)
}

#[cfg(test)]
mod tests;
