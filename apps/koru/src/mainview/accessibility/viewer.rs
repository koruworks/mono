// SPDX-License-Identifier: MPL-2.0

//! **The media viewer as a client hears it** (chat-attachments D11,
//! ADR-482).
//!
//! The viewer answers its own semantics as plain data
//! (`koru_ui::media_viewer::MediaViewer::semantics`): the dialog's name, a
//! picture's size, the zoom, and every button on offer with its label and
//! rect. This file only projects that into the window's tree and routes a
//! client's press back, so the viewer is a full dialog rather than the empty
//! one every other blocker publishes.
//!
//! The dialog keeps the blocker's identity, `shell#media-viewer`, so a client
//! matching on the kind of thing standing finds it where it always was; its
//! controls are `s:media-viewer#<action>`, one keyword per action, the same
//! in every language.

use accesskit::{Action as A11yAction, Role};
use koru_ui::media_viewer::{Action, ViewerControl, ViewerSemantics};
use koru_workspace_host::accessibility::{ControlPart, ObjectKey, Occurrence, SemanticNode};

use super::Shell;
use crate::mainview::MainView;

/// The viewer's controls, as a surface of their own.
const VIEWER: &str = "media-viewer";

/// Each action's keyword, the part a client addresses it by.
fn part(action: Action) -> ControlPart {
    ControlPart::new(match action {
        Action::ZoomOut => "zoom-out",
        Action::ZoomIn => "zoom-in",
        Action::Fit => "fit",
        Action::ActualSize => "zoom",
        Action::Copy => "copy",
        Action::SaveAs => "save-as",
        Action::OpenExternally => "open-externally",
        Action::Close => "close",
    })
}

const ACTIONS: [Action; 8] = [
    Action::ZoomOut,
    Action::ZoomIn,
    Action::Fit,
    Action::ActualSize,
    Action::Copy,
    Action::SaveAs,
    Action::OpenExternally,
    Action::Close,
];

/// The viewer action a client named, if it named one of the viewer's
/// controls.
pub(super) fn action_of(target: Occurrence) -> Option<Action> {
    if target.object != ObjectKey::Surface(VIEWER) {
        return None;
    }
    ACTIONS
        .into_iter()
        .find(|action| part(*action).name() == target.part.name())
}

fn control(cx: &Shell, control: &ViewerControl, role: Role) -> SemanticNode {
    let mut node = SemanticNode::new(cx.surface(VIEWER, part(control.action)), role)
        .with_label(control.label.clone())
        .with_bounds(control.rect)
        .with_action(A11yAction::Click);
    if let Some(value) = &control.value {
        node = node.with_value(value.clone());
    }
    node
}

/// The dialog, its zoom readout and its buttons. `dialog` is the
/// blocker's own occurrence.
pub(super) fn project(cx: &Shell, dialog: Occurrence, semantics: ViewerSemantics) -> SemanticNode {
    let mut children = vec![control(cx, &semantics.zoom, Role::Button)];
    children.extend(
        semantics
            .buttons
            .iter()
            .map(|button| control(cx, button, Role::Button)),
    );
    let mut node = SemanticNode::new(dialog, Role::Dialog)
        .with_label(semantics.name)
        .with_bounds(semantics.bounds)
        .modal()
        .with_children(children);
    if let Some(description) = semantics.description {
        node = node.with_value(description);
    }
    node
}

impl MainView {
    /// The standing viewer's semantics, whichever surface owns it.
    pub(super) fn media_viewer_semantics(&self, cx: &Shell) -> Option<ViewerSemantics> {
        self.lifted_viewer_semantics(cx.lang)
    }

    /// A client pressed one of the viewer's controls: do what the pointer
    /// would have.
    pub(super) fn media_viewer_action(&mut self, target: Occurrence, action: A11yAction) -> bool {
        if action != A11yAction::Click {
            return false;
        }
        let Some(viewer_action) = action_of(target) else {
            return false;
        };
        self.lifted_viewer_act(viewer_action)
    }
}
