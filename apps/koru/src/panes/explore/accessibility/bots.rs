// SPDX-License-Identifier: MPL-2.0

//! **The Bots roster, as a screen reader sees it** — the Chats view's model
//! (`chats.rs`) for a list whose rows are Bots.
//!
//! One list node named *Bots*, and one item per Bot in the roster's order,
//! named by the Bot (and *paused* when it is), offering **Open** as its
//! click. The line over the list — loading, or the service not answering —
//! and the empty roster's sentence are labels. There is no button: the
//! roster offers none yet.
//!
//! **Geometry is never re-derived.** Every rect is the roster's own element
//! frame, the one its paint and its pointer read; a row scrolled out of the
//! band is published where it would be.

use accesskit::{Action, Role};
use koru_draw::geometry::Rect;
use koru_i18n::{Key, t};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, SemanticNode,
};

use super::Context;
use crate::bots::BotsRoster;
use crate::bots::roster::{EMPTY, LIST, NOTICE, row_key};
use crate::panes::Outcome;

/// The list, and the lines of it that are not a Bot.
const BOTS: &str = "explore-bots";
const NOTICE_PART: ControlPart = ControlPart::new("notice");
const EMPTY_PART: ControlPart = ControlPart::new("empty");

fn bot_occurrence(cx: &Context, bot: koru_ai_core::bots::BotId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, bot.as_uuid(), ControlPart::ROOT)
}

/// The roster's nodes: the line over it, when there is one, and the list or
/// the empty sentence.
pub(super) fn semantics(roster: &BotsRoster, cx: &Context, top: f32) -> Vec<SemanticNode> {
    roster.with_input_frame(cx.area, top, |_, frame| {
        let rect_of = |key| frame.node(key).map(|node| node.rect);
        let mut out = Vec::new();
        if let Some(rect) = rect_of(NOTICE) {
            let words = match roster.load() {
                crate::bots::roster::Load::Unavailable => Key::BotsUnavailable,
                _ => Key::BotsLoading,
            };
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, NOTICE_PART),
                    Role::Label,
                )
                .with_label(t(cx.lang, words))
                .with_bounds(rect),
            );
        }
        if let Some(rect) = rect_of(EMPTY) {
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, EMPTY_PART),
                    Role::Label,
                )
                .with_label(t(cx.lang, Key::BotsEmpty))
                .with_bounds(rect),
            );
        }
        if let Some(band) = rect_of(LIST) {
            let total = roster.bots().len();
            let children = roster
                .bots()
                .iter()
                .enumerate()
                .filter_map(|(index, bot)| {
                    let rect = rect_of(row_key(bot.id))?;
                    Some(
                        SemanticNode::new(bot_occurrence(cx, bot.id), Role::ListItem)
                            .with_label(roster.accessible_name(bot))
                            .with_bounds(rect)
                            .with_selected(roster.selected() == Some(bot.id))
                            .with_position(index)
                            .with_size_of_set(total)
                            .with_actions([Action::Focus, Action::Click, Action::ScrollIntoView]),
                    )
                })
                .collect();
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, ControlPart::ROOT),
                    Role::List,
                )
                .with_label(t(cx.lang, Key::BotsTitle))
                .with_bounds(band)
                .with_size_of_set(total)
                .with_children(children),
            );
        }
        out
    })
}

/// Where the keyboard is, read off the same focus the ring is drawn from.
pub(super) fn focus_occurrence(roster: &BotsRoster, cx: &Context) -> Option<Occurrence> {
    roster.focus().map(|bot| bot_occurrence(cx, bot))
}

/// Everything the published tree is built from.
pub(super) fn revise(roster: &BotsRoster, rev: &mut Revision) {
    rev.byte(roster.load() as u8);
    match roster.focus() {
        Some(bot) => rev.byte(2).uuid(bot.as_uuid()),
        None => rev.byte(0),
    };
    match roster.selected() {
        Some(bot) => rev.byte(2).uuid(bot.as_uuid()),
        None => rev.byte(0),
    };
    rev.u64(roster.bots().len() as u64);
    for bot in roster.bots() {
        rev.uuid(bot.id.as_uuid()).str(&bot.name).bool(bot.paused);
    }
}

/// **Do what a client asked, through the gesture it names.** `None` when the
/// target is not one of this list's Bots.
pub(super) fn action(
    roster: &mut BotsRoster,
    area: Rect,
    target: Occurrence,
    action: Action,
) -> Option<Outcome> {
    if area.w <= 0.0 {
        return None;
    }
    let ObjectKey::Model(id) = target.object else {
        return None;
    };
    let bot = roster
        .bots()
        .iter()
        .map(|bot| bot.id)
        .find(|bot| bot.as_uuid() == id)?;
    match action {
        // The press sets the focus; the release asks — the pair Windows
        // sends behaves like a click.
        Action::Focus | Action::ScrollIntoView => {
            roster.set_focus(bot);
            Some(Outcome::Consumed)
        }
        Action::Click => {
            roster.ask_open(bot);
            Some(Outcome::Consumed)
        }
        _ => None,
    }
}
