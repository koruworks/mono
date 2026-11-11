// SPDX-License-Identifier: MPL-2.0

//! **Bots** in the workspace window (Bots P1.8): the navigator's view of the
//! person's own Bots in the window's workspace.
//!
//! The roster, and a Bot's tab ([`BotView`]): pressing a Bot opens or
//! focuses its tab, its header over its main conversation, and the header's
//! *Settings* opens the Bot's settings ([`settings`], S5). Its ⋯ archives
//! it or moves it to Trash, a conversation row moves one conversation to
//! Trash, the roster's Archived filter unarchives, and the window's Trash
//! restores ([`lifecycle`], [`trash`], S14). Its agent and model settings
//! and creating one land in later increments
//! (`docs/features/bots/P1.8.PLAN.final.md`); until they do, nothing offers
//! a control that would lead nowhere — no *Create a bot*, and no permanent
//! delete of a Bot, which is not built.
//!
//! Two rules it is built under, the Chats view's:
//!
//! - **Nothing touches the network on the window thread.** The Bot service is
//!   asked by [`link`]'s own thread; the window only folds what came back.
//!   Under `cfg(test)` there is no thread at all.
//! - **A hidden roster does no work.** It asks nothing and lays nothing out
//!   while another view is showing; being shown again asks once.

pub(crate) mod catalog;
pub(crate) mod labels;
pub(crate) mod lifecycle;
pub(crate) mod link;
pub(crate) mod roster;
pub(crate) mod settings;
pub(crate) mod trash;
pub(crate) mod view;

pub(crate) use roster::BotsRoster;
pub(crate) use view::BotView;

/// **Whether this build shows Bots**: `koru_features::bots`, where the rule
/// is written once so a change of mind is one line, and where the Agent's
/// chat-link gate reads it too (`koru_features::chat_links`). Bots follow
/// **agents being enabled, and nothing else** (QUESTIONS O32, the owner's
/// answer, 2026-11-10): not the Koru AI switch, which shows the person's own
/// AI chat, and not the Agent-views preference (Bots ROADMAP P1.8: no flag
/// of its own). A Bot's tab and its conversation open wherever this answers
/// `true`; the window's Koru AI gates let a Bot's conversation through
/// (`mainview/bots.rs`, `MainView::chat_door_open`).
pub(crate) fn shown() -> bool {
    koru_features::bots()
}

#[cfg(test)]
pub(crate) mod tests;
