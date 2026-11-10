// SPDX-License-Identifier: MPL-2.0

//! **Bots** in the workspace window (Bots P1.8): the navigator's view of the
//! person's own Bots in the window's workspace.
//!
//! This is the roster and nothing else yet. A Bot's own view, its
//! conversations and creating one land in later increments
//! (`docs/features/bots/P1.8.PLAN.final.md`); until they do, the roster
//! offers no control that would lead nowhere — no *Create a bot*, no menu —
//! and pressing a Bot only records which one was asked for
//! ([`BotsRoster::take_wants_open`]), for the Bot view to take.
//!
//! Two rules it is built under, the Chats view's:
//!
//! - **Nothing touches the network on the window thread.** The Bot service is
//!   asked by [`link`]'s own thread; the window only folds what came back.
//!   Under `cfg(test)` there is no thread at all.
//! - **A hidden roster does no work.** It asks nothing and lays nothing out
//!   while another view is showing; being shown again asks once.

pub(crate) mod link;
pub(crate) mod roster;

pub(crate) use roster::BotsRoster;

/// **Whether this build shows Bots** — the one place the rule is written,
/// so a change of mind is one line. Bots run through Koru AI's shared chat
/// and an installed coding agent, so they are shown wherever both agents and
/// Koru AI are, **independent of the Agent-views preference** (Bots
/// ROADMAP P1.8: no flag of its own and no dependency on Agent views).
pub(crate) fn shown() -> bool {
    koru_features::agents() && koru_features::ai()
}

#[cfg(test)]
mod tests;
