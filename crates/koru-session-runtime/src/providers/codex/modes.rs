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

use koru_session_protocol::{AgentMode, AgentModeId, AgentModes, ModelLabel};
use serde_json::{Value, json};

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

/// What one mode sends to the app-server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Policy {
    /// `approvalPolicy`, on both `thread/start` and `turn/start`.
    pub approval: &'static str,
    /// `sandbox`, `thread/start`'s spelling.
    pub sandbox: &'static str,
    /// `approvalsReviewer`.
    pub reviewer: &'static str,
}

impl Policy {
    /// The mode's settings. `None` — no mode chosen — is *Ask for approval*,
    /// Koru's default for Codex; an id that is not one of the three is
    /// refused rather than guessed.
    pub(super) fn of(mode: Option<&AgentModeId>) -> Option<Self> {
        let ask = Self {
            approval: "on-request",
            sandbox: "read-only",
            reviewer: "user",
        };
        match mode.map(AgentModeId::as_str) {
            None | Some(ASK) => Some(ask),
            Some(AUTO_REVIEW) => Some(Self {
                sandbox: "workspace-write",
                reviewer: "auto_review",
                ..ask
            }),
            Some(FULL_ACCESS) => Some(Self {
                approval: "never",
                sandbox: "danger-full-access",
                ..ask
            }),
            Some(_) => None,
        }
    }

    /// What `thread/start` answered against what was asked. Not a refusal:
    /// on Windows, without Codex's own sandbox set up, 0.166.0 answers a
    /// `workspace-write` thread with a read-only sandbox, and an older
    /// app-server ignores `approvalsReviewer` and keeps the person as the
    /// reviewer. Both only ever ask more, never less, so the thread runs
    /// and the difference is logged.
    pub(super) fn observe_thread(&self, thread: &Value) {
        let reviewer = thread["approvalsReviewer"].as_str();
        let sandbox = thread.pointer("/sandbox/type").and_then(Value::as_str);
        let expected = match self.sandbox {
            "workspace-write" => "workspaceWrite",
            "danger-full-access" => "dangerFullAccess",
            _ => "readOnly",
        };
        if reviewer.is_some_and(|reviewer| reviewer != self.reviewer)
            || sandbox.is_some_and(|sandbox| sandbox != expected)
        {
            tracing::warn!(target: "koru::codex", event = "codex.mode.narrowed",
                asked_sandbox = self.sandbox, sandbox = sandbox.unwrap_or("unknown"),
                asked_reviewer = self.reviewer, reviewer = reviewer.unwrap_or("unknown"));
        }
    }

    /// `turn/start`'s `sandboxPolicy` for the same sandbox.
    pub(super) fn sandbox_policy(&self) -> Value {
        match self.sandbox {
            "workspace-write" => json!({"type":"workspaceWrite"}),
            "danger-full-access" => json!({"type":"dangerFullAccess"}),
            _ => json!({"type":"readOnly"}),
        }
    }
}

#[cfg(test)]
mod tests;
