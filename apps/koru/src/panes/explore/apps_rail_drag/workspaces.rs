// SPDX-License-Identifier: MPL-2.0

//! Workspace drag shares the rail's carry, swap and easing geometry.
//! Only release publishes a Store order; a click switches on release.
//!
//! **The run is the pinned workspaces in their saved order**, the open one
//! among them where it stands. It used to be lifted to the top, so every
//! switch reshuffled the squares; now only a drag moves one. An open
//! workspace that is not pinned is drawn after the run and is not carried:
//! it is not part of the saved order, and a drop could not keep it there.

use super::*;
use crate::panes::{Outcome, explore::DRAG_ARM_PX};
use koru_core::WorkspaceId;

#[cfg(test)]
mod tests;

pub(crate) struct WorkspaceDrag {
    id: WorkspaceId,
    original: Vec<WorkspaceId>,
    preview: Vec<WorkspaceId>,
    origin_y: f32,
    y: f32,
    grab: f32,
    armed: bool,
}

impl ExploreView {
    /// The pinned squares, in the saved order: the part of the run a drag
    /// may reorder.
    fn pinned_workspace_run(&self) -> Vec<WorkspaceId> {
        self.workspaces
            .iter()
            .filter(|w| w.pinned && !w.archived && w.deleted_at.is_none())
            .take(3)
            .map(|w| w.id)
            .collect()
    }

    /// Every workspace square, top to bottom: the pinned run, then the open
    /// workspace when it is not in it.
    fn saved_workspace_run(&self) -> Vec<WorkspaceId> {
        let mut ids = self.pinned_workspace_run();
        if let Some(current) = self.current
            && !ids.contains(&current)
        {
            ids.push(current);
        }
        ids
    }

    /// Where the run starts among the rail's squares: after the waiting
    /// square, when no workspace is open.
    fn workspace_run_offset(&self) -> usize {
        usize::from(self.current.is_none())
    }

    pub(crate) fn rail_workspace_ids(&self) -> Vec<WorkspaceId> {
        self.workspace_drag
            .as_ref()
            .map_or_else(|| self.saved_workspace_run(), |drag| drag.preview.clone())
    }

    pub(crate) fn check_workspace_drag_membership(&mut self) {
        let current = self.saved_workspace_run();
        if self.workspace_drag.as_ref().is_some_and(|drag| {
            drag.original.len() != current.len()
                || drag.original.iter().any(|id| !current.contains(id))
        }) {
            self.workspace_drag = None;
            self.workspace_slides.clear();
        }
    }

    pub(crate) fn begin_workspace_drag(
        &mut self,
        id: WorkspaceId,
        rects: &apps_rail::Rects,
        y: f32,
    ) {
        // Only a pinned square is carried. The one square that is not — the
        // open workspace drawn after the run — has nothing to switch to
        // either, so a press on it starts nothing at all.
        if !self.pinned_workspace_run().contains(&id) {
            return;
        }
        let Some((_, square)) = rects.workspaces.iter().find(|(wid, _)| *wid == Some(id)) else {
            return;
        };
        let original = self.saved_workspace_run();
        self.workspace_drag = Some(WorkspaceDrag {
            id,
            preview: original.clone(),
            original,
            origin_y: y,
            y,
            grab: y - square.y,
            armed: false,
        });
    }

    /// The pinned squares' rects, which bound a carry and are its slots.
    fn pinned_squares<'a>(&self, rects: &'a apps_rail::Rects) -> &'a [(Option<WorkspaceId>, Rect)] {
        let start = self.workspace_run_offset().min(rects.workspaces.len());
        let end = (start + self.pinned_workspace_run().len()).min(rects.workspaces.len());
        &rects.workspaces[start..end]
    }

    pub(crate) fn workspace_carried(&self, rects: &apps_rail::Rects) -> Option<(usize, Rect)> {
        let drag = self.workspace_drag.as_ref()?;
        let index = rects
            .workspaces
            .iter()
            .position(|(id, _)| *id == Some(drag.id))?;
        let square = rects.workspaces[index].1;
        let pinned = self.pinned_squares(rects);
        let first = pinned.first()?.1;
        let last = pinned.last()?.1;
        let top = if drag.armed {
            reorder::carry(drag.y - drag.grab, first.y, last.y + last.h, square.h)
        } else {
            square.y
        };
        Some((index, Rect::new(square.x, top, square.w, square.h)))
    }

    pub(crate) fn workspace_drag_move(&mut self, rail: Rect, y: f32) -> bool {
        self.check_workspace_drag_membership();
        let Some(drag) = self.workspace_drag.as_mut() else {
            return false;
        };
        drag.y = y;
        drag.armed |= (y - drag.origin_y).abs() >= DRAG_ARM_PX;
        if !drag.armed {
            return true;
        }
        let rects = self.apps_rail_rects(rail);
        let offset = self.workspace_run_offset();
        if let Some((index, carried)) = self.workspace_carried(&rects) {
            let slots: Vec<_> = self
                .pinned_squares(&rects)
                .iter()
                .map(|(_, rect)| (rect.y, rect.h))
                .collect();
            let to =
                reorder::drop_index(&slots, index - offset, carried.y + carried.h * 0.5) + offset;
            if to != index {
                let drag = self.workspace_drag.as_mut().unwrap();
                let id = drag.preview.remove(index - offset);
                drag.preview.insert(to - offset, id);
                let before: Vec<_> = rects.workspaces.iter().map(|(_, r)| r.y).collect();
                let after: Vec<_> = self
                    .apps_rail_rects(rail)
                    .workspaces
                    .iter()
                    .map(|(_, r)| r.y)
                    .collect();
                self.workspace_slides
                    .moved(&before, &after, index, to, Instant::now());
            }
        }
        true
    }

    pub(crate) fn release_workspace_drag(&mut self, rail: Rect) -> Option<Outcome> {
        self.workspace_drag.as_ref()?;
        self.check_workspace_drag_membership();
        let rects = self.apps_rail_rects(rail);
        let landing = self.workspace_carried(&rects);
        let drag = self.workspace_drag.take()?;
        if !drag.armed {
            // A click on the open workspace is already answered.
            if self.current == Some(drag.id) {
                return Some(Outcome::Consumed);
            }
            return Some(Outcome::SwitchWorkspace(drag.id));
        }
        if let Some((index, carried)) = landing {
            self.workspace_slides.settle(
                rects.workspaces.len(),
                index,
                carried.y - rects.workspaces[index].1.y,
                Instant::now(),
            );
        }
        if drag.preview == drag.original {
            return Some(Outcome::Consumed);
        }
        let at = drag.preview.iter().position(|id| *id == drag.id)?;
        // The next *pinned* square, so an open workspace drawn after the run
        // cannot become an anchor the saved order does not hold.
        let pinned = self.pinned_workspace_run().len();
        Some(Outcome::ReorderWorkspace {
            workspace: drag.id,
            before: (at + 1 < pinned)
                .then(|| drag.preview.get(at + 1).copied())
                .flatten(),
        })
    }

    pub(crate) fn nudge_workspace(&self, workspace: WorkspaceId, up: bool) -> Option<Outcome> {
        let ids = self.pinned_workspace_run();
        let at = ids.iter().position(|id| *id == workspace)?;
        let before = if up {
            if at == 0 {
                return None;
            }
            Some(ids[at - 1])
        } else {
            if at + 1 >= ids.len() {
                return None;
            }
            ids.get(at + 2).copied()
        };
        Some(Outcome::ReorderWorkspace { workspace, before })
    }
}
