// SPDX-License-Identifier: MPL-2.0

//! **The Chats view, as a screen reader sees it** — the Tasks rail's model
//! (`rail/accessibility.rs`) for a list whose rows are chats.
//!
//! One list node, one node per chat, named by its title, who answered and
//! when (and *private* when it is); *New chat* is the heading's button, in
//! the pane's own tree (the CTO's note). Each
//! chat row offers **Open** (its click) and carries a **Delete chat** button as
//! its child — the contract's two row actions (`docs/features/koruui/CONTRIBUTING.md`).
//! Geometry is never re-derived: every rect is the surface's own
//! [`layout`](koru_chats_ui::ChatsSurface::layout), unculled, so a row
//! scrolled out of the band is still published where it would be.
//!
//! **An action is the gesture it names.** Open and Delete reach the same
//! `ask_open` / `ask_delete` the pointer and keyboard reach, and come back as
//! the same [`Outcome`]s — so Delete still goes through the container's
//! confirmation.

use accesskit::{Action, Role};
use koru_chats_ui::{ChatsSurface, Focus};
use koru_draw::geometry::Rect;
use koru_i18n::{Key, t};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, SemanticNode,
};

use super::Context;
use crate::panes::Outcome;

/// The list, and the rows of it that are not a chat.
const CHATS: &str = "explore-chats";
/// The Delete button under a chat's row node.
const DELETE: ControlPart = ControlPart::new("delete");

fn chat_occurrence(cx: &Context, chat: koru_chats_ui::ChatId, part: ControlPart) -> Occurrence {
    Occurrence::model(cx.window, cx.view, chat.as_uuid(), part)
}

/// The list node and everything in it.
pub(super) fn semantics(chats: &ChatsSurface, cx: &Context, top: f32) -> SemanticNode {
    let focus = chats.focus();
    let mut children = Vec::new();
    let rows = chats.chat_rects(cx.area, top);
    let total = rows.len();
    for (index, (chat, rect)) in rows.into_iter().enumerate() {
        let name = chats.accessible_name(chat).unwrap_or_default();
        let delete = SemanticNode::new(chat_occurrence(cx, chat, DELETE), Role::Button)
            .with_label(t(cx.lang, Key::ChatsDelete))
            .with_bounds(rect)
            .with_action(Action::Click);
        children.push(
            SemanticNode::new(chat_occurrence(cx, chat, ControlPart::ROOT), Role::ListItem)
                .with_label(name)
                .with_bounds(rect)
                .with_selected(focus == Some(Focus::Chat(chat)))
                .with_position(index)
                .with_size_of_set(total)
                .with_actions([Action::Focus, Action::Click, Action::ScrollIntoView])
                .with_children(vec![delete]),
        );
    }
    SemanticNode::new(
        Occurrence::surface(cx.window, cx.view, CHATS, ControlPart::ROOT),
        Role::List,
    )
    .with_label(t(cx.lang, Key::ChatsTitle))
    .with_bounds(chats.band(cx.area, top))
    .with_size_of_set(total)
    .with_children(children)
}

/// Where the keyboard is, said as an occurrence — read off the same focus the
/// ring is drawn from.
pub(super) fn focus_occurrence(chats: &ChatsSurface, cx: &Context) -> Option<Occurrence> {
    let Focus::Chat(chat) = chats.focus()?;
    Some(chat_occurrence(cx, chat, ControlPart::ROOT))
}

/// Everything the published tree is built from. The relative times are not
/// here: they drift with the clock, and a clock is not a reason to rebuild a
/// tree every frame.
pub(super) fn revise(chats: &ChatsSurface, rev: &mut Revision) {
    rev.byte(chats.load() as u8);
    match chats.focus() {
        Some(Focus::Chat(chat)) => rev.byte(2).uuid(chat.as_uuid()),
        None => rev.byte(0),
    };
    rev.u64(chats.chats().len() as u64);
    for chat in chats.chats() {
        rev.uuid(chat.id.as_uuid())
            .str(&chat.title)
            .bool(chat.private)
            .str(chat.current_model.provider.as_str())
            .str(&chat.current_model.requested_model)
            .bool(chats.deleting(chat.id).is_some());
    }
    rev.u64(chats.agents().len() as u64);
}

/// **Do what a client asked, through the gesture it names.** `None` when the
/// target is not this list's.
pub(super) fn action(
    chats: &mut ChatsSurface,
    area: Rect,
    top: f32,
    target: Occurrence,
    action: Action,
) -> Option<Outcome> {
    if area.w <= 0.0 {
        return None;
    }
    match target.object {
        ObjectKey::Model(id) => {
            let chat = chats
                .chats()
                .iter()
                .map(|chat| chat.id)
                .find(|chat| chat.as_uuid() == id)?;
            if target.part == DELETE {
                if action != Action::Click {
                    return None;
                }
                chats.ask_delete(chat);
                let asked = chats.take_wants_delete();
                return (!asked.is_empty()).then_some(Outcome::DeleteAiChats(asked));
            }
            match action {
                // The press sets the focus; the release opens — the pair
                // Windows sends behaves like a click.
                Action::Focus => {
                    chats.set_focus(Focus::Chat(chat), area, top);
                    Some(Outcome::Consumed)
                }
                Action::Click => {
                    chats.ask_open(chat);
                    chats.take_wants_open().map(Outcome::OpenAiChat)
                }
                Action::ScrollIntoView => {
                    chats.scroll_to_chat(chat, area, top);
                    Some(Outcome::Consumed)
                }
                _ => None,
            }
        }
        _ => None,
    }
}
