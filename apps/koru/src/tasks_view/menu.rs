// SPDX-License-Identifier: MPL-2.0

//! The two menus this surface opens: a row's, and the page's overflow.
//!
//! **The rows live here; the rendering does not.** Building the row menu on
//! `mainview::context_menu` rather than on a menu of its own is what gives it
//! the red destructive row, the button anchoring and the disabled-with-a-reason
//! channel for free — and keeps every menu in the window looking like the same
//! menu. The Trash tab's `menu.rs` is the same file for the same reason.
//!
//! **A whole-row click opens the node; everything else lives here.** That is
//! the design's one sentence about this surface, and it is why there are no
//! inline edit affordances on the row itself — no due picker hanging off the
//! chip, no priority popover on the lozenge.
//!
//! ## Why the overflow menu is the view's own and the row menu is not
//!
//! Every row of the overflow changes **this page's query and nothing else**: no
//! tab set, no `&mut WorkspaceDoc`, no document anybody has to go and find. The
//! container's [`MenuAction`] vocabulary is shared with the document view and
//! the explore column, so putting *Sort by modified* in it would spend a name
//! every other menu in the window has to walk past — the same argument that
//! kept a date value off `TaskDue`.
//!
//! It is also a **setting menu**, and those behave differently: a block menu
//! closes on choice because the choice was the point, and a setting menu stays
//! open because the check has to be seen to move and because two settings are
//! usually changed together. The container's menu closes on every choice, in
//! four places; the widget underneath does not care, and
//! [`koru_ui::menu::Menu::items_mut`] exists for exactly this. So this one is
//! driven directly, on `koru-search-ui`'s app-menu precedent.

use std::borrow::Cow;

use koru_core::ids::ListId;
use koru_core::{DocumentId, NodeId};
use koru_store_wire::tasks::TaskSort;

use super::query::Filters;
use super::view::ListFace;
use crate::mainview::context_menu::{MenuAction, MenuSubject};
use crate::panes::explore::rail::{Bucket, RailTarget};

/// One task row's menu: the places it leads, three ways to change it, and the
/// one way to take it off this list.
///
/// `completed` swaps the first verb between *Complete* and *Reopen* — the same
/// control the tick is, spelled as a sentence. `list` is the list the row is in
/// when this page is not already that list's own, and it puts *Go to list*
/// beside *View in origin*.
/// **The menu over several selected tasks**: what applies to every one of
/// them, and nothing that names one — no *Open document*, no *Go to list*.
/// *Add document* leads when any of them was typed into this page and so
/// has no document; *Reopen* replaces *Complete* when every one is done.
pub fn rows_menu(
    tasks: Vec<(DocumentId, NodeId)>,
    native: Vec<(DocumentId, NodeId)>,
    completed: bool,
) -> (MenuSubject, Vec<MenuAction>) {
    let mut items = Vec::new();
    if !native.is_empty() {
        items.push(MenuAction::TaskCreateDocument);
        items.push(MenuAction::Divider);
    }
    items.extend([
        if completed {
            MenuAction::TaskReopen
        } else {
            MenuAction::TaskComplete
        },
        MenuAction::TaskDue,
        MenuAction::TaskPriority,
        MenuAction::TaskList,
        MenuAction::Divider,
        MenuAction::TaskRemove,
    ]);
    (MenuSubject::Tasks { tasks, native }, items)
}

pub fn row_menu(
    document: DocumentId,
    node: NodeId,
    completed: bool,
    native: bool,
    list: Option<ListId>,
    pinned_today: bool,
) -> (MenuSubject, Vec<MenuAction>) {
    // **A task typed into this page has no origin to view**, and is offered a
    // document to make instead. It lives in the workspace's hidden tasks
    // document — the row says *Added here, no document* — and following it
    // there would open a page nobody made; *Create document* gives it one.
    // The group is the places the row leads, or could.
    let mut items = vec![if native {
        MenuAction::TaskCreateDocument
    } else {
        MenuAction::TaskGoToOrigin
    }];
    if list.is_some() {
        items.push(MenuAction::TaskGoToList);
    }
    items.push(MenuAction::Divider);
    items.extend([
        if completed {
            MenuAction::TaskReopen
        } else {
            MenuAction::TaskComplete
        },
        // **Beside the three that change the task**, because it is a fourth:
        // it says where the task shows, not what it is. It sits under
        // Complete/Reopen and above the date, since *on Today* is a claim
        // about now and the due is a claim about later.
        if pinned_today {
            MenuAction::TaskUnpinToday
        } else {
            MenuAction::TaskPinToday
        },
        MenuAction::TaskDue,
        MenuAction::TaskPriority,
        MenuAction::TaskList,
        // **Hand it to an agent** (ROADMAP D7). Below the four edits and
        // above the hairline, because it is something you do *with* the task
        // rather than something you change about it.
        MenuAction::TaskDelegate,
        MenuAction::Divider,
        MenuAction::TaskRemove,
    ]);
    (MenuSubject::Task { document, node }, items)
}

// ------------------------------------------------------ the membership picker

/// One row of the membership picker.
///
/// **Its own surface, not an editor picker.** `Due…` and `Priority…` go to the
/// node and open the editor's picker over it, because the editor *has* one; a
/// list has no picker there and could not have a useful one — `/attr` would
/// offer `koru.list` with no values, since the values live in the workspace's
/// system document rather than in the attribute registry (D-4). So this is a
/// plain [`koru_ui::menu::Menu`] at the view level, on the overflow menu's
/// precedent, and what it dispatches is a
/// [`TaskCommand`](koru_workspace_host::tasks::TaskCommand) like every other
/// change this surface makes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ListPick {
    /// One of the workspace's lists, checked when the task is already in it,
    /// wearing the swatch the rail draws for it.
    List {
        id: ListId,
        name: String,
        color: Option<String>,
        checked: bool,
    },
    Divider,
    /// Take the task out of whatever list it is in. **"Clear", not "None"** —
    /// the word the colour and priority pickers already use for removing a
    /// value, and the last row for the same reason.
    Clear,
    /// The workspace has no lists yet. Never chosen; it says where one is made,
    /// because a picker offering only *Clear* is a dead end wearing a menu.
    Empty,
}

/// The picker's rows: every list in the table's order, then *Clear*.
///
/// The divider is written only when there is something above it — a panel that
/// opens on a rule reads as a menu missing its first row.
pub fn list_picker_items(lists: &[ListFace], current: Option<ListId>) -> Vec<ListPick> {
    if lists.is_empty() {
        return vec![ListPick::Empty];
    }
    let mut items: Vec<ListPick> = lists
        .iter()
        .map(|face| ListPick::List {
            id: face.id,
            name: face.name.clone(),
            color: face.color.clone(),
            checked: current == Some(face.id),
        })
        .collect();
    items.push(ListPick::Divider);
    items.push(ListPick::Clear);
    items
}

impl koru_ui::MenuItem for ListPick {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::List { name, .. } => Cow::Borrowed(name.as_str()),
            Self::Divider => Cow::Borrowed(""),
            Self::Clear => Cow::Borrowed("Clear"),
            Self::Empty => Cow::Borrowed("No lists yet \u{2014} make one in the sidebar"),
        }
    }

    /// Only a list row carries a check. *Clear* is a verb, and a tick beside it
    /// would claim the task is in a list called Clear.
    fn checked(&self) -> Option<bool> {
        match self {
            Self::List { checked, .. } => Some(*checked),
            _ => None,
        }
    }

    /// The list's colour ahead of its name — the same square the rail draws,
    /// so a list is one colour everywhere it is named.
    fn swatch(&self) -> Option<Option<&str>> {
        match self {
            Self::List { color, .. } => Some(color.as_deref()),
            _ => None,
        }
    }

    fn divider(&self) -> bool {
        matches!(self, Self::Divider)
    }

    fn selectable(&self) -> bool {
        !matches!(self, Self::Divider | Self::Empty)
    }

    fn height(&self) -> f32 {
        match self {
            Self::Divider => 8.0,
            _ => koru_draw::theme::ROW_H,
        }
    }
}

/// What a row of the overflow menu is.
///
/// **Exactly the three things that change what the query returns, plus the sort
/// that orders it.** The design says so in as many words, and it is the rule
/// that decides what may ever be added here: a control that changes what is
/// *drawn* rather than what is *asked* is a chip in the open, where it can be
/// read without pressing anything.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum OverflowRow {
    /// **Keep what was just finished on the page it was finished on** —
    /// [`Filters::recently_completed`].
    ///
    /// The one switch the menu carries now. Three lived here before it and
    /// all three were the same question asked badly: two of them said *show
    /// completed* on a page that is completed, and the third asked about a
    /// window the page could not show you the far side of. What a person
    /// actually wants is for the thing they ticked a minute ago not to
    /// vanish, and that is what this is.
    RecentlyCompleted,
    Divider,
    /// The quiet caption over the three sort rows. Never chosen.
    SortHeading,
    Sort(TaskSort),
}

impl OverflowRow {
    /// The rows, in the design's order. **One list, used to build the menu, to
    /// answer what is enabled, and to assert what is in it** — a fourth item
    /// added anywhere shows up in all three.
    pub const ROWS: [Self; 6] = [
        Self::RecentlyCompleted,
        Self::Divider,
        Self::SortHeading,
        Self::Sort(TaskSort::Due),
        Self::Sort(TaskSort::Priority),
        Self::Sort(TaskSort::Modified),
    ];

    /// A queue page's rows — Today, Next, Upcoming, Later: the same, with
    /// the dragged order offered last (the owner's ask, 2026-11-05). A queue
    /// opens by due date; *Manual* is a choice, and the order it shows is the
    /// one rank every page reads, so a task moved up here is moved up in its
    /// list too.
    pub const QUEUE_ROWS: [Self; 7] = [
        Self::RecentlyCompleted,
        Self::Divider,
        Self::SortHeading,
        Self::Sort(TaskSort::Due),
        Self::Sort(TaskSort::Priority),
        Self::Sort(TaskSort::Modified),
        Self::Sort(TaskSort::Manual),
    ];

    /// A list page's rows: the same, with the order somebody dragged it
    /// into first — the order a list page opens in.
    pub const LIST_ROWS: [Self; 7] = [
        Self::RecentlyCompleted,
        Self::Divider,
        Self::SortHeading,
        Self::Sort(TaskSort::Manual),
        Self::Sort(TaskSort::Due),
        Self::Sort(TaskSort::Priority),
        Self::Sort(TaskSort::Modified),
    ];

    /// The rows `target`'s page offers.
    pub fn rows(target: RailTarget) -> &'static [Self] {
        match target {
            RailTarget::List(_) => &Self::LIST_ROWS,
            // The archive is ordered by when things were finished, and has
            // nothing to drag into an order of its own.
            RailTarget::Bucket(Bucket::Completed) => &Self::ROWS,
            _ => &Self::QUEUE_ROWS,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::RecentlyCompleted => "Show recently completed",
            Self::Divider => "",
            Self::SortHeading => "Sort",
            Self::Sort(TaskSort::Due) => "Due date",
            Self::Sort(TaskSort::Priority) => "Priority",
            Self::Sort(TaskSort::Modified) => "Modified",
            Self::Sort(TaskSort::Manual) => "Manual",
        }
    }

    /// Whether this row is a check at all, and — when the state is in hand —
    /// whether it is set. A row with no check answer reserves no check column.
    fn checked(self, filters: &Filters, target: RailTarget) -> Option<bool> {
        match self {
            Self::RecentlyCompleted => Some(filters.recently_completed),
            Self::Sort(sort) => Some(filters.sort_for(target) == sort),
            Self::Divider | Self::SortHeading => None,
        }
    }
}

/// One row as the widget sees it: what it is, and what its check said **when the
/// menu was built**.
///
/// The check rides on the item rather than being asked of the view each frame
/// for the reason the container's menus fix their rows at open time — a panel
/// whose contents are recomputed under the pointer can relabel itself between
/// the press and the release. It moves in one place: [`sync`](Self::sync),
/// after a row was chosen and the menu deliberately stayed open.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct OverflowItem {
    pub row: OverflowRow,
    pub checked: Option<bool>,
}

/// The whole menu, built against the filters as they stand.
pub fn overflow_items(filters: &Filters, target: RailTarget) -> Vec<OverflowItem> {
    OverflowRow::rows(target)
        .iter()
        .map(|row| OverflowItem {
            row: *row,
            checked: row.checked(filters, target),
        })
        .collect()
}

/// Refresh the checks in place. The rows and their widths do not move — only
/// the marks — which is what lets the panel stay open under the pointer.
pub fn sync(items: &mut [OverflowItem], filters: &Filters, target: RailTarget) {
    for item in items.iter_mut() {
        item.checked = item.row.checked(filters, target);
    }
}

impl koru_ui::MenuItem for OverflowItem {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.row.label())
    }

    fn checked(&self) -> Option<bool> {
        self.checked
    }

    fn divider(&self) -> bool {
        self.row == OverflowRow::Divider
    }

    fn selectable(&self) -> bool {
        !matches!(self.row, OverflowRow::Divider | OverflowRow::SortHeading)
    }

    fn height(&self) -> f32 {
        match self.row {
            // A touch shorter than a real row, so the caption sits as a heading
            // — the container's `SelectionHeader` arithmetic, and the same
            // reason: a heading the height of a row reads as a row you cannot
            // press.
            OverflowRow::SortHeading => koru_draw::theme::ROW_H - 4.0,
            OverflowRow::Divider => 8.0,
            _ => koru_draw::theme::ROW_H,
        }
    }

    /// Why *Show completed inline* is dimmed on a queue page.
    ///
    /// **A bucket names its own lifecycle**, so the store discards the field
    /// rather than ANDing it (`TaskQuery::bucket`) — the row would do nothing at
    /// all. A dimmed row that does not explain itself is a dead end, and this is
    /// the one surface here with a channel for saying why.
    fn disabled_reason(&self) -> Option<Cow<'static, str>> {
        (self.row == OverflowRow::RecentlyCompleted)
            .then_some(Cow::Borrowed("Everything on this page is completed"))
    }
}

/// One row of the colour picker: the shared accent colours, the line's own
/// one checked, then *Clear* — the editor's colour chooser, as a menu.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ColorPick {
    Color { token: &'static str, checked: bool },
    Divider,
    Clear,
}

/// The colour picker's rows: every accent colour in its own order, then
/// *Clear* while the line has one.
pub fn color_picker_items(current: Option<&str>) -> Vec<ColorPick> {
    let mut items: Vec<ColorPick> = koru_core::attributes::AccentColor::ALL
        .into_iter()
        .map(|color| ColorPick::Color {
            token: color.as_str(),
            checked: current == Some(color.as_str()),
        })
        .collect();
    if current.is_some() {
        items.push(ColorPick::Divider);
        items.push(ColorPick::Clear);
    }
    items
}

impl koru_ui::MenuItem for ColorPick {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Color { token, .. } => {
                let mut name = token.to_string();
                if let Some(first) = name.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                Cow::Owned(name)
            }
            Self::Divider => Cow::Borrowed(""),
            Self::Clear => Cow::Borrowed("Clear"),
        }
    }

    fn checked(&self) -> Option<bool> {
        match self {
            Self::Color { checked, .. } => Some(*checked),
            _ => None,
        }
    }

    /// The colour itself ahead of its name.
    fn swatch(&self) -> Option<Option<&str>> {
        match self {
            Self::Color { token, .. } => Some(Some(token)),
            _ => None,
        }
    }

    fn divider(&self) -> bool {
        matches!(self, Self::Divider)
    }
}
