// SPDX-License-Identifier: MPL-2.0

//! The continuation input of a parent's successor after a child wait
//! (contract 4.5).
//!
//! Built for either kind of parent Run, interactive or Routine. Every field
//! but the key and the text is copied from the parent's original admission
//! input. The text holds the parent's own captured task and, per request,
//! three things: the request id, the recipient Bot's id and one closed
//! outcome token. It holds no page content, title, answer text or task text
//! of a child: the successor reads content with `read_bot_request`, so every
//! byte of content passes the parent's own read check.
//!
//! The text is deterministic over retained rows, so the fingerprint a
//! reservation binds equals the fingerprint of the input sent later: every
//! request is settled before a reservation, and a settled request never
//! changes.

use super::*;
use koru_delegate_core::children::{ChildRequestState, ChildSettledBy};
use koru_delegate_core::decisions::DecisionDigest;
use koru_delegate_core::{AgentTurnId, BotAdmissionState, PendingInput, SessionId};
use std::path::PathBuf;

/// Domain of the continuation fingerprint a child reservation binds.
const CONTINUATION_DOMAIN: &str = "koru.bot.child.continuation.v1";
/// Domain of the digest of one observed continuation acknowledgment.
const ACKNOWLEDGMENT_DOMAIN: &str = "koru.bot.child.continuation.ack.v1";
/// The native tool a resumed parent calls to read one request's result.
const READ_REQUEST_TOOL: &str = "read_bot_request";

/// The closed token a continuation names for one settled request. It is the
/// ledger's own record of who settled the request; a child whose admission
/// was refused before it ran is named `failed`. How a child that ended did
/// end (completed, canceled, failed) is the child Run's own terminal status,
/// which the reader of the request reports.
pub(super) fn outcome_token(receipt: &ChildRequestReceipt) -> Option<&'static str> {
    if receipt.state != ChildRequestState::Settled {
        return None;
    }
    Some(match receipt.settled_by? {
        ChildSettledBy::TimedOut => "timed_out",
        ChildSettledBy::ParentStopped => "parent_stopped",
        ChildSettledBy::ChildEnded if receipt.child == BotAdmissionState::Refused => "failed",
        ChildSettledBy::ChildEnded => "child_ended",
    })
}

/// Provider payload, never logged: the parent's original captured task
/// input, why a new session continues it, and one line of ids and a token
/// per request.
pub(super) fn continuation_body(
    task: &str,
    requests: &[ChildRequestReceipt],
) -> Result<String, RunError> {
    let mut lines = String::new();
    for receipt in requests {
        let token = outcome_token(receipt).ok_or_else(|| refused("bot_fenced"))?;
        lines.push_str(&format!(
            "\n- request_id `{request}`, bot_id `{bot}`: {token}",
            request = receipt.child_request,
            bot = receipt.recipient,
        ));
    }
    Ok(format!(
        "{task}\n\nKoru resumed this run in a new session after it asked other Bots. The earlier session's conversation is not available here; results it already wrote remain.\n\nEvery request has ended. For each one, call `{READ_REQUEST_TOOL}` with its request_id to read what that Bot saved, then continue the task. A request that is not `child_ended` has no result to read.\n{lines}"
    ))
}

/// The digest of one positive host acknowledgment of the continuation input:
/// its retained key and fingerprint, the bound session and the accepted turn.
pub(super) fn acknowledgment_digest(
    key: IdempotencyKey,
    fingerprint: [u8; 32],
    session: SessionId,
    turn: AgentTurnId,
) -> Result<DecisionDigest, RunError> {
    let encoded = serde_json::to_vec(&(ACKNOWLEDGMENT_DOMAIN, key, fingerprint, session, turn))
        .map_err(|_| refused("bot_invalid"))?;
    Ok(DecisionDigest::from_bytes(
        *blake3::hash(&encoded).as_bytes(),
    ))
}

impl Orchestrator {
    /// The fingerprint a successor reservation binds for this Run's open
    /// child wait: the exact continuation input it will send, less its key.
    pub fn bot_child_continuation_fingerprint(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<[u8; 32], RunError> {
        let execution = self.child_ticket(owner, run)?;
        Ok(self
            .child_continuation(owner, run, &execution, IdempotencyKey::mint())?
            .1)
    }

    /// The exact continuation input under `key` and its key-free
    /// fingerprint. The parent's original captured task input only: no
    /// recapture, no assembled history, no attachment of the first session.
    pub(super) fn child_continuation(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &ChildWaitExecution,
        key: IdempotencyKey,
    ) -> Result<(PendingInput, [u8; 32]), RunError> {
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let submission = &admission.submission;
        if submission.provenance.owner != owner
            || submission.request.run != run
            || execution.open_requests != 0
        {
            return Err(refused("bot_fenced"));
        }
        let workspace = self.child_run_workspace(run)?;
        let task = &submission.input;
        let text = crate::orchestrator::bot_controls::native_prompt::compose(
            run,
            &workspace,
            &continuation_body(&task.text, &execution.requests)?,
        );
        let input = PendingInput {
            idempotency_key: key,
            text,
            attachment_ids: Vec::new(),
            references: task.references.clone(),
            mode: task.mode.clone(),
            fast: task.fast,
            reasoning: task.reasoning.clone(),
            skills: task.skills.clone(),
            model: task.model.clone(),
        };
        let encoded = serde_json::to_vec(&(
            CONTINUATION_DOMAIN,
            execution.wait.wait,
            &input.text,
            &input.attachment_ids,
            &input.references,
            &input.mode,
            &input.fast,
            &input.reasoning,
            &input.skills,
            &input.model,
        ))
        .map_err(|_| refused("bot_invalid"))?;
        Ok((input, *blake3::hash(&encoded).as_bytes()))
    }

    /// The Run's working folder, as the original attempt's Bot instructions
    /// named it. Unknown refuses; no folder is guessed.
    pub(super) fn child_run_workspace(&mut self, run: RunId) -> Result<PathBuf, RunError> {
        if let Some(path) = self
            .ports
            .mcp
            .run_workspace(run)?
            .filter(|path| !path.as_os_str().is_empty())
        {
            return Ok(path);
        }
        self.runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .map(|attempt| attempt.run_workspace.clone())
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| refused("bot_folder_unavailable"))
    }
}
