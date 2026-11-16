// SPDX-License-Identifier: MPL-2.0

//! The rail's keyboard position and its accessibility projection: the five
//! task queues and the tree of lists, both over the rail's own row geometry.
//! The inline name field and the row menu remain the container's surfaces. No
//! Store query or task operation belongs in this projection or its callbacks.
//!
//! One ring, in the order the rows are drawn: the four queues, the `+`
//! beside *Lists*, every list that is showing, then Completed. A list row
//! adds what a tree row has — Left and Right shut and open it, as in the
//! document tree — and the menu key opens its menu, which is how a list is
//! made inside another, moved to the top level or deleted without a pointer.

use accesskit::{Action, Role};
use koru_core::ids::ListId;
use koru_i18n::{Key, t};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, SemanticNode,
};
use winit::keyboard::{Key as WKey, NamedKey};

use super::{Bucket, FOOTER_PAD, RailFocus, RailRow, RailTarget, TasksRail, TasksRailRequest};
use crate::panes::{Outcome, explore::accessibility::Context};
use koru_draw::geometry::Rect;

const QUEUE: &str = "tasks-queue";
const LISTS: &str = "tasks-lists";
const NEW_LIST: ControlPart = ControlPart::new("new-list");
const ALL: [Bucket; 5] = [
    Bucket::Today,
    Bucket::Next,
    Bucket::Upcoming,
    Bucket::Later,
    Bucket::Completed,
];

fn token(bucket: Bucket) -> &'static str {
    match bucket {
        Bucket::Today => "today",
        Bucket::Next => "next",
        Bucket::Upcoming => "upcoming",
        Bucket::Later => "later",
        Bucket::Completed => "completed",
    }
}

fn label(bucket: Bucket) -> Key {
    match bucket {
        Bucket::Today => Key::A11yTaskBucketToday,
        Bucket::Next => Key::A11yTaskBucketNext,
        Bucket::Upcoming => Key::A11yTaskBucketUpcoming,
        Bucket::Later => Key::A11yTaskBucketLater,
        Bucket::Completed => Key::A11yTaskBucketCompleted,
    }
}

fn occurrence(cx: &Context, bucket: Bucket) -> Occurrence {
    Occurrence::surface(cx.window, cx.view, QUEUE, ControlPart::new(token(bucket)))
}

fn list_occurrence(cx: &Context, id: ListId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, id.as_uuid(), ControlPart::ROOT)
}

impl TasksRail {
    pub(crate) fn queue_semantics(&self, cx: &Context, top: f32) -> SemanticNode {
        let children = self
            .laid_out(cx.area, top)
            .into_iter()
            .filter_map(|(row, bounds)| {
                let RailRow::Bucket(bucket) = row else {
                    return None;
                };
                Some(
                    SemanticNode::new(occurrence(cx, bucket), Role::Tab)
                        .with_label(t(cx.lang, label(bucket)))
                        .with_bounds(bounds)
                        .with_selected(self.selected == Some(RailTarget::Bucket(bucket)))
                        .with_position(ALL.iter().position(|b| *b == bucket).unwrap())
                        .with_actions([Action::Focus, Action::Click, Action::ScrollIntoView]),
                )
            })
            .collect();
        SemanticNode::new(
            Occurrence::surface(cx.window, cx.view, QUEUE, ControlPart::ROOT),
            Role::TabList,
        )
        .with_label(t(cx.lang, Key::A11yTasksHeading))
        .with_bounds(Rect::new(
            cx.area.x,
            top,
            cx.area.w,
            (cx.area.y + cx.area.h - FOOTER_PAD - top).max(0.0),
        ))
        .with_size_of_set(ALL.len())
        .with_children(children)
    }

    /// The Lists section: a tree named *Lists* holding the `+` and one item
    /// per list that is showing.
    ///
    /// **A row's level is its real depth**, not the capped indent it is drawn
    /// at (nested-lists D7). A parent advertises Expand or Collapse, the
    /// document tree's convention — including its note that `expanded` itself
    /// is left unset, because setting it costs the row its Invoke pattern on
    /// Windows and gives a client nothing back today.
    pub(crate) fn lists_semantics(&self, cx: &Context, top: f32) -> SemanticNode {
        let mut children = Vec::new();
        let mut band: Option<Rect> = None;
        let mut position = 0;
        for (row, bounds) in self.laid_out(cx.area, top) {
            match row {
                RailRow::ListsHeading => {
                    band = Some(bounds);
                    let plus = self.row_parts(row, bounds).trailing;
                    children.push(
                        SemanticNode::new(
                            Occurrence::surface(cx.window, cx.view, LISTS, NEW_LIST),
                            Role::Button,
                        )
                        .with_label(t(cx.lang, Key::TaskListNew))
                        .with_bounds(plus)
                        .with_actions([Action::Focus, Action::Click]),
                    );
                }
                RailRow::List(i) => {
                    let Some(list) = self.lists.get(i) else {
                        continue;
                    };
                    if let Some(band) = band.as_mut() {
                        band.h = bounds.y + bounds.h - band.y;
                    }
                    let mut node = SemanticNode::new(list_occurrence(cx, list.id), Role::TreeItem)
                        .with_label(list.name.clone())
                        .with_bounds(bounds)
                        .with_level(list.depth + 1)
                        .with_position(position)
                        .with_actions([Action::Click, Action::Focus, Action::ScrollIntoView]);
                    position += 1;
                    // `Some(true)` on the open page's row and nothing on the
                    // rest: a node carrying `selected` loses Invoke.
                    if self.selected == Some(RailTarget::List(list.id)) {
                        node = node.with_selected(true);
                    }
                    if list.has_children {
                        node = node.with_action(if self.is_open(list.id) {
                            Action::Collapse
                        } else {
                            Action::Expand
                        });
                    }
                    children.push(node);
                }
                _ => {}
            }
        }
        SemanticNode::new(
            Occurrence::surface(cx.window, cx.view, LISTS, ControlPart::ROOT),
            Role::Tree,
        )
        .with_label(t(cx.lang, Key::TaskListsHeading))
        .with_bounds(band.unwrap_or(Rect::new(cx.area.x, top, cx.area.w, 0.0)))
        .with_size_of_set(position)
        .with_children(children)
    }

    pub(crate) fn queue_focus_occurrence(&self, cx: &Context) -> Option<Occurrence> {
        Some(match self.focus? {
            RailFocus::Bucket(bucket) => occurrence(cx, bucket),
            RailFocus::NewList => Occurrence::surface(cx.window, cx.view, LISTS, NEW_LIST),
            RailFocus::List(id) => list_occurrence(cx, id),
        })
    }

    pub(crate) fn revise_queues(&self, rev: &mut Revision) {
        match self.focus {
            Some(RailFocus::Bucket(bucket)) => rev.str(token(bucket)),
            Some(RailFocus::NewList) => rev.str("new-list"),
            Some(RailFocus::List(id)) => rev.str(&id.to_storage()),
            None => rev.str("none"),
        };
        match self.selected {
            Some(RailTarget::Bucket(bucket)) => rev.str(token(bucket)),
            Some(RailTarget::List(id)) => rev.str(&id.to_storage()),
            _ => rev.str("none"),
        };
        rev.u64(self.lists.len() as u64);
        // Names, places and which rows are open are all in the projection.
        for &i in &self.visible {
            let list = &self.lists[i];
            rev.str(&list.id.to_storage());
            rev.str(&list.name);
            rev.u64(list.depth as u64);
            rev.u64(u64::from(list.has_children && self.is_open(list.id)));
        }
        rev.u64(u64::from(self.scroll.to_bits()));
    }

    pub(crate) fn has_queue_focus(&self) -> bool {
        self.focus.is_some()
    }

    pub(crate) fn clear_queue_focus(&mut self) -> bool {
        self.focus.take().is_some()
    }

    pub(crate) fn focus_queues(&mut self, area: Rect, top: f32) -> bool {
        if area.w <= 0.0 {
            return false;
        }
        let focus = match self.selected {
            Some(RailTarget::Bucket(bucket)) => RailFocus::Bucket(bucket),
            Some(RailTarget::List(id)) if self.row_of_list(id).is_some() => RailFocus::List(id),
            _ => RailFocus::Bucket(Bucket::Today),
        };
        self.set_focus(area, top, focus);
        true
    }

    /// Put the keyboard on a row, and bring that row into view. What the
    /// container calls to hand focus back after a field or a dialog closes
    /// (nested-lists §6).
    pub(crate) fn set_focus(&mut self, area: Rect, top: f32, focus: RailFocus) {
        self.focus = Some(focus);
        match focus {
            RailFocus::Bucket(bucket) => {
                self.reveal(area, top, RailRow::Bucket(bucket));
            }
            RailFocus::NewList => {
                self.reveal(area, top, RailRow::ListsHeading);
            }
            RailFocus::List(id) => {
                self.reveal_list(area, top, id);
            }
        }
    }

    pub(crate) fn focus(&self) -> Option<RailFocus> {
        self.focus
    }

    /// The list drawn above `id`, or the `+` when it is the first: where the
    /// keyboard goes when `id` is deleted.
    pub(crate) fn focus_above(&self, id: ListId) -> RailFocus {
        let at = self
            .visible
            .iter()
            .position(|&i| self.lists[i].id == id)
            .unwrap_or(0);
        match at.checked_sub(1).map(|slot| self.visible[slot]) {
            Some(i) => RailFocus::List(self.lists[i].id),
            None => RailFocus::NewList,
        }
    }

    /// Every stop of the ring, in the order the rows are drawn.
    fn ring(&self) -> Vec<RailFocus> {
        let mut ring: Vec<RailFocus> = super::QUEUES.into_iter().map(RailFocus::Bucket).collect();
        ring.push(RailFocus::NewList);
        ring.extend(
            self.visible
                .iter()
                .map(|&i| RailFocus::List(self.lists[i].id)),
        );
        ring.push(RailFocus::Bucket(Bucket::Completed));
        ring
    }

    /// The same intent pointer, keyboard and AT activation produce.
    pub(crate) fn open_bucket(&mut self, bucket: Bucket, new_tab: bool) -> TasksRailRequest {
        if !new_tab {
            self.selected = Some(RailTarget::Bucket(bucket));
        }
        self.focus = None;
        TasksRailRequest::OpenBucket { bucket, new_tab }
    }

    /// A list's page, opened the way a press on its row opens it.
    pub(crate) fn open_list(&mut self, id: ListId, new_tab: bool) -> TasksRailRequest {
        if !new_tab {
            self.selected = Some(RailTarget::List(id));
        }
        self.focus = None;
        TasksRailRequest::OpenList { id, new_tab }
    }

    pub(crate) fn queue_action(
        &mut self,
        area: Rect,
        top: f32,
        target: Occurrence,
        action: Action,
    ) -> Option<Outcome> {
        if area.w <= 0.0 {
            return None;
        }
        let focus = match target.object {
            ObjectKey::Surface(QUEUE) => RailFocus::Bucket(
                ALL.into_iter()
                    .find(|bucket| token(*bucket) == target.part.name())?,
            ),
            ObjectKey::Surface(LISTS) if target.part == NEW_LIST => RailFocus::NewList,
            ObjectKey::Model(uuid) => {
                RailFocus::List(self.lists.iter().find(|list| list.id.as_uuid() == uuid)?.id)
            }
            _ => return None,
        };
        match (action, focus) {
            (Action::Focus, _) => {
                self.set_focus(area, top, focus);
                Some(Outcome::Consumed)
            }
            (Action::ScrollIntoView, _) => {
                let keep = self.focus;
                self.set_focus(area, top, focus);
                self.focus = keep;
                Some(Outcome::Consumed)
            }
            (Action::Click, _) => Some(self.activate(focus)),
            (Action::Expand, RailFocus::List(id)) => {
                self.set_open(id, true);
                Some(Outcome::Consumed)
            }
            (Action::Collapse, RailFocus::List(id)) => {
                self.set_open(id, false);
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    /// Enter, Space or an AT click on a stop of the ring.
    fn activate(&mut self, focus: RailFocus) -> Outcome {
        Outcome::Tasks(match focus {
            RailFocus::Bucket(bucket) => self.open_bucket(bucket, false),
            RailFocus::NewList => TasksRailRequest::BeginNewList { parent: None },
            RailFocus::List(id) => self.open_list(id, false),
        })
    }

    pub(crate) fn queue_key(&mut self, area: Rect, top: f32, key: &WKey) -> Option<Outcome> {
        let focus = self.focus?;
        let ring = self.ring();
        let index = ring.iter().position(|stop| *stop == focus).unwrap_or(0);
        let next = match key {
            WKey::Named(NamedKey::ArrowDown) => (index + 1).min(ring.len() - 1),
            WKey::Named(NamedKey::ArrowUp) => index.saturating_sub(1),
            WKey::Named(NamedKey::Home) => 0,
            WKey::Named(NamedKey::End) => ring.len() - 1,
            WKey::Named(NamedKey::Enter) | WKey::Named(NamedKey::Space) => {
                return Some(self.activate(focus));
            }
            WKey::Named(NamedKey::Escape) | WKey::Named(NamedKey::Tab) => {
                self.focus = None;
                return Some(Outcome::Consumed);
            }
            // The document tree's keys, on a list row: Right opens a shut
            // parent and then steps inside it; Left shuts an open one and
            // then steps out to the list it is inside.
            WKey::Named(NamedKey::ArrowRight) | WKey::Named(NamedKey::ArrowLeft) => {
                let RailFocus::List(id) = focus else {
                    return None;
                };
                let list = self.lists.iter().find(|list| list.id == id)?.clone();
                let right = matches!(key, WKey::Named(NamedKey::ArrowRight));
                let open = list.has_children && self.is_open(id);
                if list.has_children && right != open {
                    self.set_open(id, right);
                    self.set_focus(area, top, focus);
                    return Some(Outcome::Consumed);
                }
                let step = if right {
                    open.then(|| ring.get(index + 1).copied()).flatten()
                } else {
                    list.parent.map(RailFocus::List)
                };
                if let Some(step) = step {
                    self.set_focus(area, top, step);
                }
                return Some(Outcome::Consumed);
            }
            // The menu key: the row's menu, hung from the row itself.
            WKey::Named(NamedKey::ContextMenu) => {
                let (target, want) = match focus {
                    RailFocus::Bucket(bucket) => {
                        (RailTarget::Bucket(bucket), RailRow::Bucket(bucket))
                    }
                    RailFocus::List(id) => (RailTarget::List(id), self.row_of_list(id)?),
                    RailFocus::NewList => return None,
                };
                let (_, r) = self
                    .rows(area, top)
                    .into_iter()
                    .find(|(row, _)| *row == want)?;
                let label = self.row_parts(want, r).label;
                return Some(Outcome::Tasks(TasksRailRequest::RowMenu {
                    target,
                    at: (label.x, r.y + r.h),
                }));
            }
            _ => return None,
        };
        self.set_focus(area, top, ring[next]);
        Some(Outcome::Consumed)
    }
}

#[cfg(test)]
mod tests;
