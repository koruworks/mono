// SPDX-License-Identifier: MPL-2.0

//! **The approval modes Koru gives Codex** (koru-ai D7). The app-server
//! advertises no modes of its own; it takes an approval policy, a sandbox
//! and a reviewer on `thread/start` and on each `turn/start`. Koru offers
//! three, and this module alone knows what each one sends:
//!
//! | id | approval | sandbox | reviewer |
//! | --- | --- | --- | --- |
//! | `ask` (the default) | `on-request` | read-only | the person |
//! | `auto-review` | `on-request` | workspace-write | Codex's reviewer |
//! | `full-access` | `never` | none | — |
//!
//! Sending them overrides what the person's `config.toml` says (decided,
//! 2026-11-02). The reviewer is a per-thread and per-turn setting of the
//! app-server (`approvalsReviewer`, seen on 0.166.0), not a launch flag.

use koru_session_protocol::{AgentMode, AgentModes, ModelLabel};

/// Codex reads; it asks before editing files or running commands.
pub(super) const ASK: &str = "ask";
/// Codex edits in its folder; its reviewer decides anything that needs more.
pub(super) const AUTO_REVIEW: &str = "auto-review";
/// No sandbox: Codex runs everything without asking.
pub(super) const FULL_ACCESS: &str = "full-access";

/// The three, in the picker's order, *Ask for approval* current.
pub(super) fn offered() -> AgentModes {
    let mode = |id: &str, name: &str| {
        let mut mode = AgentMode::new(id.parse().expect("a static mode id"));
        mode.name = ModelLabel::name(name);
        mode
    };
    AgentModes::new(
        vec![
            mode(ASK, "Ask for approval"),
            mode(AUTO_REVIEW, "Approve for me"),
            mode(FULL_ACCESS, "Full access"),
        ],
        ASK.parse().ok(),
    )
    .expect("three static modes")
}

#[cfg(test)]
mod tests;
