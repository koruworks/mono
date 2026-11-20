// SPDX-License-Identifier: MPL-2.0

//! **The Bots roster, as a screen reader sees it** — the Chats view's model
//! (`chats.rs`) for a list whose rows are Bots.
//!
//! One list node named *Bots*, and one item per Bot in the roster's order,
//! named by the Bot (and *paused* when it is), offering **Open** as its
//! click. The line over the list — loading, or the service not answering —
//! and the empty roster's sentence are labels. Its one button is the
//! Archived filter's toggle (S14); open, the filter is a list named
//! *Archived* whose items open their Bot and carry *Unarchive {name}* when
//! the window knows the Archive.
//!
//! **A Bot's running and Needs-you conversations** (S10) are items under its
//! item, each named by which conversation and its state's words — never a
//! color alone — offering **Open** as its click, which opens that
//! conversation in the Bot's tab. The sentence saying the read stopped
//! before the end is a label.
//!
//! **Creating one** (S8): the empty roster's *Create a bot* is a button.
//! The Unfinished group is a list named *Unfinished* whose items are named
//! *{name}, not finished*, each with its state as a label and *Retry
//! {name}* (when Retry can resolve it) and *Abandon {name}* as buttons; an
//! Abandon's answer is labels, with *Open role page* as a button when it
//! left one.
//!
//! **The person's other computers** (M8, P8.3): one list per computer whose
//! Bots this one keeps a copy of, named *On {computer}, {state}*. Its items
//! are that computer's Bots, each described *Runs on {computer}* and
//! offering **Open** as its click; before them, as labels, that its Koru
//! needs an update and how many Bots it does not copy, and *Refresh the
//! copy from {computer}* as a button when the roster offers it.
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
use crate::bots::roster::{
    ABANDONED_LINES, ACTIVITY_PART, ARCHIVED_HEADING, ARCHIVED_NOTICE, ARCHIVED_PART,
    ARCHIVED_TOGGLE, CREATE, CREATION_ABANDON_PART, CREATION_PART, CREATION_RETRY_PART, EMPTY,
    LIST, MORE_WORK, NOTICE, OPEN_LEFT_PAGE, TEAM_CONTROL, TEAM_LINE, UNARCHIVE_PART,
    UNFINISHED_HEADING, UNFINISHED_MORE, UNFINISHED_NOTICE, activity_key, archived_key,
    creation_abandon_key, creation_key, creation_retry_key, creation_state_key, row_key,
    unarchive_key,
};
use crate::panes::Outcome;

/// The list, and the lines of it that are not a Bot.
const BOTS: &str = "explore-bots";
const NOTICE_PART: ControlPart = ControlPart::new("notice");
const EMPTY_PART: ControlPart = ControlPart::new("empty");
const MORE_WORK_PART: ControlPart = ControlPart::new("more-work");
/// *Pause all Bots* / *Resume all Bots* and the line under it (M8).
const TEAM_PART: ControlPart = ControlPart::new("team-pause");
const TEAM_LINE_PART: ControlPart = ControlPart::new("team-pause-line");
const ACTIVITY: ControlPart = ControlPart::new(ACTIVITY_PART);
/// The Archived filter (S14).
const ARCHIVED_TOGGLE_PART: ControlPart = ControlPart::new("archived-toggle");
const ARCHIVED_NOTICE_PART: ControlPart = ControlPart::new("archived-notice");
const ARCHIVED_LIST_PART: ControlPart = ControlPart::new("archived");
const ARCHIVED: ControlPart = ControlPart::new(ARCHIVED_PART);
const UNARCHIVE: ControlPart = ControlPart::new(UNARCHIVE_PART);
/// Creating one (S8).
const CREATE_PART: ControlPart = ControlPart::new("create");
const UNFINISHED_PART: ControlPart = ControlPart::new("unfinished");
const UNFINISHED_NOTICE_PART: ControlPart = ControlPart::new("unfinished-notice");
const UNFINISHED_MORE_PART: ControlPart = ControlPart::new("unfinished-more");
const ABANDONED_PARTS: [ControlPart; 3] = [
    ControlPart::new("abandoned"),
    ControlPart::new("abandoned-more"),
    ControlPart::new("abandoned-last"),
];
const OPEN_LEFT_PART: ControlPart = ControlPart::new("open-left-page");
const CREATION: ControlPart = ControlPart::new(CREATION_PART);
const CREATION_STATE: ControlPart = ControlPart::new("bot-creation-state");
const CREATION_RETRY: ControlPart = ControlPart::new(CREATION_RETRY_PART);
const CREATION_ABANDON: ControlPart = ControlPart::new(CREATION_ABANDON_PART);
/// Another computer's group (M8), under that computer's own id.

fn bot_occurrence(cx: &Context, bot: koru_ai_core::bots::BotId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, bot.as_uuid(), ControlPart::ROOT)
}

/// The roster's nodes as the column publishes them, in a 280 × 900 pane at
/// the window's origin. For the tests that check a roster's tree is one
/// AccessKit can take (`crate::bots::tests::assert_publishable`).
#[cfg(test)]
pub(crate) fn roster_tree(roster: &BotsRoster) -> Vec<SemanticNode> {
    use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};
    let cx = Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(1),
        area: Rect::new(48.0, 0.0, 280.0, 900.0),
        rail: Rect::new(0.0, 0.0, 48.0, 900.0),
        viewport: (1200.0, 900.0),
        lang: koru_i18n::Lang::En,
    };
    semantics(roster, &cx, 0.0)
}

/// The roster's nodes: the line over it, when there is one, and the list or
/// the empty sentence.
pub(super) fn semantics(roster: &BotsRoster, cx: &Context, top: f32) -> Vec<SemanticNode> {
    roster.with_input_frame(cx.area, top, |_, frame| {
        // **Where it shows, inside the list's viewport**: a row scrolled
        // out of view was published at the place it would be, over the
        // column's title or under the Trash row, and found there by a point
        // (the fourth window run). Clipped as its pointer region is
        // (`Node::hit_region`); a row wholly out of view keeps an empty box
        // at the viewport's edge, and Scroll into view still reveals it.
        let rect_of = |key| frame.node(key).map(|node| shown_part(node.rect, node.clip));
        let mut out = Vec::new();
        if let Some(rect) = rect_of(NOTICE) {
            // What the roster draws there, from the one place that says it.
            let words = roster.notice().unwrap_or(Key::BotsLoading);
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, NOTICE_PART),
                    Role::Label,
                )
                .with_label(t(cx.lang, words))
                .with_bounds(rect),
            );
        }
        // **Pause all Bots** (M8): a button named by what a press does, with
        // no action and marked busy while a change waits to be confirmed;
        // and the line under it.
        if let (Some(rect), Some(control)) = (rect_of(TEAM_CONTROL), roster.team().control()) {
            let node = SemanticNode::new(
                Occurrence::surface(cx.window, cx.view, BOTS, TEAM_PART),
                Role::Button,
            )
            .with_label(t(cx.lang, control.words))
            .with_bounds(rect);
            out.push(if control.enabled {
                node.with_actions([Action::Click])
            } else {
                node.with_busy()
            });
        }
        if let (Some(rect), Some(words)) = (rect_of(TEAM_LINE), roster.team().line()) {
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, TEAM_LINE_PART),
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
                .with_label(t(cx.lang, Key::BotsNoneYet))
                .with_bounds(rect),
            );
        }
        if let Some(rect) = rect_of(CREATE) {
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, CREATE_PART),
                    Role::Button,
                )
                .with_label(t(cx.lang, Key::BotsCreate))
                .with_bounds(rect)
                .with_actions([Action::Click]),
            );
        }
        if let Some(rect) = rect_of(MORE_WORK) {
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, MORE_WORK_PART),
                    Role::Label,
                )
                .with_label(t(cx.lang, Key::BotsMoreWork))
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
                    let mut work: Vec<SemanticNode> = roster
                        .conversation_ids(bot.id)
                        .into_iter()
                        .filter_map(|chat| {
                            let rect = rect_of(activity_key(chat))?;
                            Some(
                                SemanticNode::new(
                                    Occurrence::model(cx.window, cx.view, chat.as_uuid(), ACTIVITY),
                                    Role::ListItem,
                                )
                                .with_label(roster.conversation_name(bot, chat))
                                .with_selected(roster.current_conversation() == Some(chat))
                                .with_bounds(rect)
                                .with_actions([Action::Click]),
                            )
                        })
                        .collect();
                    if let Some(bounds) = rect_of(crate::bots::roster::more_key(bot.id)) {
                        work.push(
                            SemanticNode::new(
                                Occurrence::model(
                                    cx.window,
                                    cx.view,
                                    bot.id.as_uuid(),
                                    ControlPart::new("bot-more"),
                                ),
                                Role::Button,
                            )
                            .with_label(t(cx.lang, Key::A11yMoreActions))
                            .with_bounds(bounds)
                            .with_actions([Action::Click]),
                        );
                    }
                    Some(
                        SemanticNode::new(bot_occurrence(cx, bot.id), Role::ListItem)
                            .with_label(roster.accessible_name(bot))
                            // The row's device mark, in words (O155).
                            .with_description(t(cx.lang, Key::BotsRunsOnThisComputer))
                            .with_bounds(rect)
                            .with_selected(roster.selected() == Some(bot.id))
                            .with_position(index)
                            .with_size_of_set(total)
                            .with_actions([Action::Focus, Action::Click, Action::ScrollIntoView])
                            .with_children(work),
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
        // **The Unfinished group** (S8): a list of the creations that have
        // not finished, each with its state, Retry and Abandon; before it,
        // why the last of those did not happen, and what an Abandon left.
        if let Some(heading) = rect_of(UNFINISHED_HEADING) {
            if let (Some(rect), Some(words)) =
                (rect_of(UNFINISHED_NOTICE), roster.creation_refused())
            {
                out.push(
                    SemanticNode::new(
                        Occurrence::surface(cx.window, cx.view, BOTS, UNFINISHED_NOTICE_PART),
                        Role::Label,
                    )
                    .with_label(t(cx.lang, words))
                    .with_bounds(rect),
                );
            }
            if let Some(abandoned) = roster.abandoned() {
                let lines = crate::bots::view::create::abandoned_lines(cx.lang, abandoned);
                for ((words, key), part) in
                    lines.into_iter().zip(ABANDONED_LINES).zip(ABANDONED_PARTS)
                {
                    if let Some(rect) = rect_of(key) {
                        out.push(
                            SemanticNode::new(
                                Occurrence::surface(cx.window, cx.view, BOTS, part),
                                Role::Label,
                            )
                            .with_label(words)
                            .with_bounds(rect),
                        );
                    }
                }
            }
            if let Some(rect) = rect_of(OPEN_LEFT_PAGE) {
                out.push(
                    SemanticNode::new(
                        Occurrence::surface(cx.window, cx.view, BOTS, OPEN_LEFT_PART),
                        Role::Button,
                    )
                    .with_label(t(cx.lang, Key::BotsOpenRole))
                    .with_bounds(rect)
                    .with_actions([Action::Click]),
                );
            }
            let total = roster.creations().len();
            let items = roster
                .creations()
                .iter()
                .enumerate()
                .filter_map(|(index, creation)| {
                    let request = creation.request_id;
                    let rect = rect_of(creation_key(request))?;
                    let name = roster.creation_name(creation);
                    let mut children = vec![
                        SemanticNode::new(
                            Occurrence::model(
                                cx.window,
                                cx.view,
                                request.as_uuid(),
                                CREATION_STATE,
                            ),
                            Role::Label,
                        )
                        .with_label(roster.creation_state(creation))
                        // Its own line under the row's head.
                        .with_bounds(rect_of(creation_state_key(request)).unwrap_or(rect)),
                    ];
                    for (key, part, words) in [
                        (
                            creation_retry_key(request),
                            CREATION_RETRY,
                            Key::BotsRetryNamed,
                        ),
                        (
                            creation_abandon_key(request),
                            CREATION_ABANDON,
                            Key::BotsAbandonNamed,
                        ),
                    ] {
                        if let Some(rect) = rect_of(key) {
                            children.push(
                                SemanticNode::new(
                                    Occurrence::model(cx.window, cx.view, request.as_uuid(), part),
                                    Role::Button,
                                )
                                .with_label(koru_i18n::fill(t(cx.lang, words), &[name.as_str()]))
                                .with_bounds(rect)
                                .with_actions([Action::Click]),
                            );
                        }
                    }
                    Some(
                        SemanticNode::new(
                            Occurrence::model(cx.window, cx.view, request.as_uuid(), CREATION),
                            Role::ListItem,
                        )
                        .with_label(roster.creation_accessible_name(creation))
                        .with_bounds(rect)
                        .with_position(index)
                        .with_size_of_set(total)
                        .with_children(children),
                    )
                })
                .collect();
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, UNFINISHED_PART),
                    Role::List,
                )
                .with_label(t(cx.lang, Key::BotsUnfinished))
                .with_bounds(heading)
                .with_size_of_set(total)
                .with_children(items),
            );
            if let Some(rect) = rect_of(UNFINISHED_MORE) {
                out.push(
                    SemanticNode::new(
                        Occurrence::surface(cx.window, cx.view, BOTS, UNFINISHED_MORE_PART),
                        Role::Label,
                    )
                    .with_label(t(cx.lang, Key::BotsUnfinishedMore))
                    .with_bounds(rect),
                );
            }
        }
        // **The Archived filter** (S14): its toggle, then — open — a list
        // named *Archived* whose items open the Bot and carry *Unarchive*.
        if let Some(rect) = rect_of(ARCHIVED_TOGGLE) {
            let words = if roster.archived_open() {
                Key::BotsHideArchived
            } else {
                Key::BotsShowArchived
            };
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, ARCHIVED_TOGGLE_PART),
                    Role::Button,
                )
                .with_label(t(cx.lang, words))
                .with_bounds(rect)
                .with_actions([Action::Click]),
            );
        }
        if let (Some(rect), Some(words)) = (rect_of(ARCHIVED_NOTICE), roster.archived_notice()) {
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, ARCHIVED_NOTICE_PART),
                    Role::Label,
                )
                .with_label(t(cx.lang, words))
                .with_bounds(rect),
            );
        }
        if let Some(heading) = rect_of(ARCHIVED_HEADING) {
            let total = roster.archived().len();
            let items = roster
                .archived()
                .iter()
                .enumerate()
                .filter_map(|(index, bot)| {
                    let rect = rect_of(archived_key(bot.id))?;
                    let unarchive = rect_of(unarchive_key(bot.id)).map(|rect| {
                        SemanticNode::new(
                            Occurrence::model(cx.window, cx.view, bot.id.as_uuid(), UNARCHIVE),
                            Role::Button,
                        )
                        .with_label(koru_i18n::fill(
                            t(cx.lang, Key::BotsUnarchiveNamed),
                            &[bot.config.name.as_str()],
                        ))
                        .with_bounds(rect)
                        .with_actions([Action::Click])
                    });
                    let menu = rect_of(crate::bots::roster::more_key(bot.id)).map(|bounds| {
                        SemanticNode::new(
                            Occurrence::model(
                                cx.window,
                                cx.view,
                                bot.id.as_uuid(),
                                ControlPart::new("bot-more"),
                            ),
                            Role::Button,
                        )
                        .with_label(t(cx.lang, Key::A11yMoreActions))
                        .with_bounds(bounds)
                        .with_actions([Action::Click])
                    });
                    Some(
                        SemanticNode::new(
                            Occurrence::model(cx.window, cx.view, bot.id.as_uuid(), ARCHIVED),
                            Role::ListItem,
                        )
                        .with_label(bot.config.name.clone())
                        .with_bounds(rect)
                        .with_position(index)
                        .with_size_of_set(total)
                        .with_actions([Action::Click])
                        .with_children(unarchive.into_iter().chain(menu).collect()),
                    )
                })
                .collect();
            out.push(
                SemanticNode::new(
                    Occurrence::surface(cx.window, cx.view, BOTS, ARCHIVED_LIST_PART),
                    Role::List,
                )
                .with_label(t(cx.lang, Key::BotsArchived))
                .with_bounds(heading)
                .with_size_of_set(total)
                .with_children(items),
            );
        }
        out
    })
}

/// **The part of `rect` its viewport shows**: all of it outside a scroll,
/// else what lies inside `clip` — an empty box at the viewport's nearer edge
/// when none does, never a box over what is drawn beside the list.
fn shown_part(rect: Rect, clip: Option<Rect>) -> Rect {
    let Some(clip) = clip else {
        return rect;
    };
    let x0 = rect.x.max(clip.x);
    let x1 = (rect.x + rect.w).min(clip.x + clip.w).max(x0);
    let y0 = rect.y.clamp(clip.y, clip.y + clip.h);
    let y1 = (rect.y + rect.h).clamp(clip.y, clip.y + clip.h).max(y0);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Where the keyboard is, read off the same focus the ring is drawn from:
/// a Bot row or one of its running and Needs-you sub-rows, *Create a bot*,
/// *Open role page*, an Unfinished row's Retry or
/// Abandon, the Archived toggle, an archived row or its Unarchive — each the
/// occurrence [`semantics`] publishes for it.
pub(super) fn focus_occurrence(roster: &BotsRoster, cx: &Context) -> Option<Occurrence> {
    let key = roster.focus()?;
    let model = |part| match key.object {
        koru_draw::el::Object::Model(id) => Some(Occurrence::model(
            cx.window,
            cx.view,
            uuid::Uuid::from_u128(id),
            part,
        )),
        _ => None,
    };
    match key.part {
        _ if key == CREATE => Some(Occurrence::surface(cx.window, cx.view, BOTS, CREATE_PART)),
        _ if key == OPEN_LEFT_PAGE => Some(Occurrence::surface(
            cx.window,
            cx.view,
            BOTS,
            OPEN_LEFT_PART,
        )),
        _ if key == ARCHIVED_TOGGLE => Some(Occurrence::surface(
            cx.window,
            cx.view,
            BOTS,
            ARCHIVED_TOGGLE_PART,
        )),
        _ if key == TEAM_CONTROL => Some(Occurrence::surface(cx.window, cx.view, BOTS, TEAM_PART)),
        crate::bots::roster::ROW_PART => model(ControlPart::ROOT),
        "bot-more" => model(ControlPart::new("bot-more")),
        ACTIVITY_PART => model(ACTIVITY),
        CREATION_RETRY_PART => model(CREATION_RETRY),
        CREATION_ABANDON_PART => model(CREATION_ABANDON),
        ARCHIVED_PART => model(ARCHIVED),
        UNARCHIVE_PART => model(UNARCHIVE),
        _ => None,
    }
}

/// Everything the published tree is built from.
pub(super) fn revise(roster: &BotsRoster, rev: &mut Revision) {
    rev.byte(roster.load() as u8);
    // A scroll moves every row's clipped bounds and nothing else.
    rev.f32(roster.scroll_offset());
    match roster.focus() {
        Some(key) => rev.byte(2).str(&format!("{key:?}")),
        None => rev.byte(0),
    };
    match roster.selected() {
        Some(bot) => rev.byte(2).uuid(bot.as_uuid()),
        None => rev.byte(0),
    };
    rev.u64(roster.bots().len() as u64);
    for bot in roster.bots() {
        rev.uuid(bot.id.as_uuid())
            .str(&bot.name)
            .bool(bot.paused)
            .bool(roster.has_unread(bot.id));
    }
    // The team pause (M8): the control's words, its line, the rows' state.
    rev.str(&format!(
        "{:?}{:?}{}",
        roster.team().state(),
        roster.team().line(),
        roster.team().holds_bots()
    ));
    rev.bool(roster.archived_open())
        .byte(roster.archived_load() as u8)
        .u64(roster.archived().len() as u64);
    for bot in roster.archived() {
        rev.uuid(bot.id.as_uuid())
            .str(&bot.config.name)
            .bool(roster.can_unarchive(bot.id))
            .bool(roster.unarchiving(bot.id));
    }
    rev.bool(roster.activity_truncated())
        .u64(roster.activity().len() as u64);
    for bot in roster.bots() {
        for chat in roster.conversation_ids(bot.id) {
            rev.uuid(chat.as_uuid())
                .str(roster.known_title(chat).unwrap_or_default())
                .bool(roster.current_conversation() == Some(chat));
        }
    }
    for entry in roster.activity() {
        rev.uuid(entry.chat_id.as_uuid())
            .byte(entry.state as u8)
            // The title an open tab shows for it: the row's words.
            .str(roster.known_title(entry.chat_id).unwrap_or_default());
    }
    // Creating one (S8).
    rev.u64(roster.creations().len() as u64);
    for creation in roster.creations() {
        rev.uuid(creation.request_id.as_uuid())
            .str(&creation.name)
            .byte(creation.stage as u8)
            .bool(creation.retryable)
            .bool(roster.creation_acting(creation.request_id));
    }
    rev.bool(roster.abandoned().is_some())
        .bool(roster.creation_refused().is_some())
        .bool(roster.creations_more());
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
    if let ObjectKey::Model(id) = target.object
        && target.part == ControlPart::new("bot-more")
        && action == Action::Click
    {
        let bot = roster
            .bots()
            .iter()
            .map(|bot| bot.id)
            .chain(roster.archived().iter().map(|bot| bot.id))
            .find(|bot| bot.as_uuid() == id)?;
        roster.press_key(crate::bots::roster::more_key(bot));
        return Some(Outcome::Consumed);
    }
    // Creating one (S8): *Create a bot*, *Open role page*, and an
    // Unfinished row's Retry and Abandon.
    if matches!(target.object, ObjectKey::Surface(_))
        && (target.part == CREATE_PART || target.part == OPEN_LEFT_PART)
    {
        if action != Action::Click {
            return None;
        }
        roster.press_unfinished(if target.part == CREATE_PART {
            CREATE
        } else {
            OPEN_LEFT_PAGE
        });
        return Some(Outcome::Consumed);
    }
    if let ObjectKey::Model(id) = target.object
        && (target.part == CREATION_RETRY || target.part == CREATION_ABANDON)
    {
        if action != Action::Click {
            return None;
        }
        let request = roster
            .creations()
            .iter()
            .map(|creation| creation.request_id)
            .find(|request| request.as_uuid() == id)?;
        roster.press_unfinished(if target.part == CREATION_RETRY {
            creation_retry_key(request)
        } else {
            creation_abandon_key(request)
        });
        return Some(Outcome::Consumed);
    }
    // *Pause all Bots* / *Resume all Bots* (M8): a press, as the pointer's.
    if target.part == TEAM_PART && matches!(target.object, ObjectKey::Surface(_)) {
        if action != Action::Click {
            return None;
        }
        roster.press_team();
        return Some(Outcome::Consumed);
    }
    // The Archived filter's toggle (S14).
    if target.part == ARCHIVED_TOGGLE_PART && matches!(target.object, ObjectKey::Surface(_)) {
        if action != Action::Click {
            return None;
        }
        roster.press_archived(ARCHIVED_TOGGLE);
        return Some(Outcome::Consumed);
    }
    let ObjectKey::Model(id) = target.object else {
        return None;
    };
    // An archived row opens its Bot; its *Unarchive* unarchives it.
    if target.part == ARCHIVED || target.part == UNARCHIVE {
        if action != Action::Click {
            return None;
        }
        let bot = roster
            .archived()
            .iter()
            .map(|bot| bot.id)
            .find(|bot| bot.as_uuid() == id)?;
        let key = if target.part == UNARCHIVE {
            unarchive_key(bot)
        } else {
            archived_key(bot)
        };
        roster.press_archived(key);
        return Some(Outcome::Consumed);
    }
    // A running or Needs-you sub-row: open that conversation.
    if target.part == ACTIVITY {
        if action != Action::Click {
            return None;
        }
        let chat = roster
            .conversation_ids_all()
            .into_iter()
            .find(|chat| chat.as_uuid() == id)?;
        roster.ask_open_chat(chat);
        return Some(Outcome::Consumed);
    }
    // A Bot of this computer's, or one another computer runs (M8): its row.
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
