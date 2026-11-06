// SPDX-License-Identifier: MPL-2.0

//! **The New… launcher, projected in full while it stands** — a menu named
//! *New*, one item per tile named by its word, placed where each is drawn.
//!
//! The place chooser's shape for the place chooser's reason
//! (`place_chooser.rs`): a short list of things to choose whose highlight
//! the arrows move is the published focus, and ids from the block
//! `Launcher::instance` reserved when it opened, so a request queued against
//! a closed launcher resolves to nothing. *Focus* moves the highlight the
//! way an arrow does, *Click* chooses the way Enter does.

use accesskit::{Action, Role};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};

use super::{Shell, instance_of, parts};
use crate::mainview::MainView;

/// One opening of the launcher.
pub(super) const LAUNCHER: &str = "launcher";

/// One tile of it.
const ITEM: ControlPart = ControlPart::new("item");

impl MainView {
    /// The launcher and its tiles, or `None` when it is not up.
    pub(super) fn launcher_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let launcher = &self.launcher.as_ref()?.launcher;
        let semantics = launcher.semantics(cx.viewport);
        let count = semantics.items.len();
        let children = semantics
            .items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                SemanticNode::new(cx.instance(LAUNCHER, item.instance, ITEM), Role::MenuItem)
                    .with_label(item.label)
                    .with_bounds(item.rect)
                    .with_actions([Action::Click, Action::Focus])
                    .with_position(i)
            })
            .collect();
        Some(
            SemanticNode::new(
                cx.instance(LAUNCHER, launcher.instance(), parts::ROOT),
                Role::Menu,
            )
            .with_label(semantics.name)
            .with_bounds(semantics.bounds)
            .with_size_of_set(count)
            .with_children(children)
            .modal(),
        )
    }

    /// The highlighted tile — what Enter would make.
    pub(super) fn launcher_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let launcher = &self.launcher.as_ref()?.launcher;
        Some(cx.instance(LAUNCHER, launcher.tile_instance(launcher.highlight()), ITEM))
    }
}

/// Whether a request names the standing launcher or one of its tiles.
pub(super) fn is_target(view: &MainView, target: Occurrence) -> bool {
    instance_of(target, LAUNCHER).is_some_and(|id| {
        view.launcher.as_ref().is_some_and(|ask| {
            ask.launcher.instance() == id || ask.launcher.tile_of_instance(id).is_some()
        })
    })
}

/// Route a request at a tile, through the launcher's own gestures.
pub(super) fn action(view: &mut MainView, target: Occurrence, action: Action) -> bool {
    let Some(id) = instance_of(target, LAUNCHER) else {
        return false;
    };
    let Some(index) = view
        .launcher
        .as_ref()
        .and_then(|ask| ask.launcher.tile_of_instance(id))
    else {
        return false;
    };
    match action {
        Action::Focus => view.set_launcher_highlight(index),
        Action::Click => view.choose_launcher_tile(index, false, false, false),
        _ => false,
    }
}
