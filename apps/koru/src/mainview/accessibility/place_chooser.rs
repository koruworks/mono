// SPDX-License-Identifier: MPL-2.0

//! **The place chooser, projected in full while it stands** — a menu named
//! *Where to open*, with one item per target named by what it does (*Split
//! up*, *Split left*, *New tab*, …), placed where each is drawn.
//!
//! The shape is the component's (`koru_ui::place_chooser::Semantics`): a
//! menu's shape for a menu's reason, a short list of things to choose whose
//! highlight the arrows move is the published focus. Its ids come from the
//! block `PlaceChooser::instance` reserved when it opened, so a request
//! queued against a closed chooser resolves to nothing — the menu's rule
//! (`menus.rs`), under a kind of its own. This file only projects it.
//!
//! An action is the gesture that already exists: *Focus* moves the highlight
//! the way an arrow does, *Click* chooses the way Enter does.

use accesskit::{Action, Role};
use koru_core::schema::WorkspaceDoc;
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};

use super::{Shell, instance_of, parts};
use crate::mainview::MainView;

/// One opening of the place chooser.
pub(super) const PLACE: &str = "place";

/// One target of it.
const ITEM: ControlPart = ControlPart::new("item");

impl MainView {
    /// The chooser and its five targets, or `None` when it is not up.
    pub(super) fn place_chooser_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let chooser = &self.place_chooser.as_ref()?.chooser;
        let semantics = chooser.semantics(cx.viewport, cx.lang);
        let count = semantics.items.len();
        let children = semantics
            .items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                SemanticNode::new(cx.instance(PLACE, item.instance, ITEM), Role::MenuItem)
                    .with_label(item.label)
                    .with_bounds(item.rect)
                    .with_actions([Action::Click, Action::Focus])
                    .with_position(i)
            })
            .collect();
        Some(
            SemanticNode::new(
                cx.instance(PLACE, chooser.instance(), parts::ROOT),
                Role::Menu,
            )
            .with_label(semantics.name)
            .with_bounds(semantics.bounds)
            .with_size_of_set(count)
            .with_children(children)
            .modal(),
        )
    }

    /// The highlighted target — what Enter would open.
    pub(super) fn place_chooser_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let chooser = &self.place_chooser.as_ref()?.chooser;
        Some(cx.instance(PLACE, chooser.place_instance(chooser.highlight()), ITEM))
    }
}

/// Whether a request names the standing chooser or one of its targets.
pub(super) fn is_target(view: &MainView, target: Occurrence) -> bool {
    instance_of(target, PLACE).is_some_and(|id| {
        view.place_chooser.as_ref().is_some_and(|ask| {
            ask.chooser.instance() == id || ask.chooser.place_of_instance(id).is_some()
        })
    })
}

/// Route a request at a target, through the chooser's own gestures.
pub(super) fn action(
    view: &mut MainView,
    ws: &WorkspaceDoc,
    target: Occurrence,
    action: Action,
) -> bool {
    let Some(id) = instance_of(target, PLACE) else {
        return false;
    };
    let Some(place) = view
        .place_chooser
        .as_ref()
        .and_then(|ask| ask.chooser.place_of_instance(id))
    else {
        return false;
    };
    match action {
        Action::Focus => view.set_place_highlight(place),
        Action::Click => view.choose_place(ws, place),
        _ => false,
    }
}
