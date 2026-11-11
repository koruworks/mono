// SPDX-License-Identifier: MPL-2.0

//! **Bots** in the workspace window (Bots P1.8): the navigator's view of the
//! person's own Bots in the window's workspace.
//!
//! The roster, and a Bot's tab ([`BotView`]): pressing a Bot opens or
//! focuses its tab, its header over its main conversation, and the header's
//! *Settings* opens the Bot's settings ([`settings`], S5). Its other
//! conversations, its agent and model settings and creating one land in
//! later increments (`docs/features/bots/P1.8.PLAN.final.md`); until they
//! do, nothing offers a control that would lead nowhere — no *Create a
//! bot*, no menu.
//!
//! Two rules it is built under, the Chats view's:
//!
//! - **Nothing touches the network on the window thread.** The Bot service is
//!   asked by [`link`]'s own thread; the window only folds what came back.
//!   Under `cfg(test)` there is no thread at all.
//! - **A hidden roster does no work.** It asks nothing and lays nothing out
//!   while another view is showing; being shown again asks once.

pub(crate) mod labels;
pub(crate) mod link;
pub(crate) mod roster;
pub(crate) mod settings;
pub(crate) mod view;

pub(crate) use roster::BotsRoster;
pub(crate) use view::BotView;

/// **Whether this build shows Bots** — the one place the rule is written,
/// so a change of mind is one line. Bots run through Koru AI's shared chat
/// and an installed coding agent, so they are shown wherever both agents and
/// Koru AI are, **independent of the Agent-views preference** (Bots
/// ROADMAP P1.8: no flag of its own and no dependency on Agent views).
pub(crate) fn shown() -> bool {
    koru_features::agents() && koru_features::ai()
}

#[cfg(test)]
pub(crate) mod tests;
