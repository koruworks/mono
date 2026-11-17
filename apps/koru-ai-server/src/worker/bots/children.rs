// SPDX-License-Identifier: MPL-2.0
//! One Bot asks other Bots and waits for their result (M6, piece W): the AI
//! worker's half.
//!
//! What lives here:
//!
//! - **The ask** (`ask.rs`, `job.rs`). A Run tool hands the worker one to
//!   three requests. The writer proves the asking Run, the page checks run off
//!   the writer (they need the Store's authority), the writer prepares the
//!   whole ask in one ai.db transaction (`prepare_bot_child_ask`), a pool job
//!   sends `RequestChild` to the delegate, and the writer stores the wait's
//!   enrollment and accepts the receipts (`accept_bot_child_ask`) **before**
//!   the tool's answer is produced. A refusal rejects every receipt with the
//!   tool code, so the same key answers the same refusal. An asked Bot runs
//!   as itself (O160): each child is captured as a task in its own
//!   conversation is, with its own Role, Knowledge and Memory plus the pages
//!   the request named, and its reply starts with its own Memory index. The
//!   capture is context; its authority is still the reduced policy.
//! - **The resend** (`resend.rs`). A child request's `start` receipt that is
//!   still `prepared` (a crash, a lost reply) is resent as `RequestChild` for
//!   its whole ask, never as its own `Submit` (contract A2).
//! - **The enrollment bridge** (`cleanup.rs`). The delegate's two private
//!   routes for a child wait, served on the writer: insert if absent, else the
//!   same digest; never `Missing` on the enroll route.
//! - **The reads** (`reads.rs`). The request cards of a parent conversation
//!   and the origin line of a child conversation.
//!
//! Nothing here holds a task, a title or a page in a log, an event or an
//! error text. The task text travels only inside the child's own submission.
use super::routine::model::RunBotSubject;
use super::*;
use koru_ai_core::bots::{BotChildAskRefusal, BotChildChat};
use koru_ai_wire::bot_child_codes as codes;
use koru_delegate_api::bots::{BotDelegatePort, BotReply};
use koru_delegate_core::ChildRequestId;
use koru_delegate_core::decisions::BotWaitId;

mod ask;
mod cleanup;
mod job;
pub(in crate::worker) mod reads;
mod resend;
#[cfg(test)]
mod tests;

pub(in crate::worker) use cleanup::CleanupIncoming;
pub(in crate::worker) use job::AskJob;
pub(in crate::worker) use reads::CardsJob;

/// How long the whole ask may take, from the tool's call to its answer
/// (contract 3.5: a finite 30-second call).
pub const CHILD_ASK_BUDGET: Duration = Duration::from_secs(30);

/// The text appended to every child's first input: the pages the request
/// named, one id per line. Always appended, so the last occurrence in a
/// stored objective is always this worker's own (a resend recovers the
/// binding's `documents` from it).
const PAGES_MARKER: &str = "\n\nPages named in this request: ";

/// One item of an ask, as the Run tool parsed it.
#[derive(Clone)]
pub struct RunChildAskItem {
    pub bot_id: BotId,
    /// The task for the asked Bot. Bounded by its type; never logged.
    pub task: koru_delegate_core::Objective,
    /// Pages the request points the child at: at most 16, each readable by
    /// the asking Bot and by the recipient at the ask.
    pub documents: Vec<koru_ai_core::DocumentId>,
}
impl std::fmt::Debug for RunChildAskItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunChildAskItem")
            .field("bot_id", &self.bot_id)
            .field("documents", &self.documents.len())
            .finish_non_exhaustive()
    }
}

/// `ask_bot`'s call to the worker: the asking Run as the gateway resolved
/// it, the model's own key for the whole call and its one to three items.
#[derive(Clone)]
pub struct RunChildAskRequest {
    pub subject: RunBotSubject,
    pub ask_key: uuid::Uuid,
    pub requests: Vec<RunChildAskItem>,
    pub deadline: Instant,
    pub cancellation: Cancellation,
}

/// The ask was admitted (or found admitted): the wait the Run is now held
/// in and the requests in input order. The gateway holds this answer until
/// the wait has left `hold_requested` (A13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunChildAskPending {
    pub wait: BotWaitId,
    pub requests: Vec<(ChildRequestId, BotId)>,
}

/// `list_askable_bots`' call: the asking Run as the gateway resolved it.
#[derive(Clone)]
pub struct RunAskableRequest {
    pub subject: RunBotSubject,
    pub deadline: Instant,
}

/// One Bot the asking Bot may name: same person, home workspace and
/// computer, active, not itself. A paused Bot is listed (asking it answers
/// `recipient_paused`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAskableBot {
    pub bot_id: BotId,
    pub name: String,
}

/// The most Bots `list_askable_bots` returns.
pub const ASKABLE_LIMIT: usize = 64;

/// What the writer is asked for by this module's client calls.
pub(in crate::worker) enum Incoming {
    Ask {
        request: RunChildAskRequest,
        reply: SyncSender<ApiResult<RunChildAskPending>>,
    },
    Askable {
        request: RunAskableRequest,
        reply: SyncSender<ApiResult<Vec<RunAskableBot>>>,
    },
    Cleanup(Box<CleanupIncoming>),
}

/// A pool job's result, back on the writer.
pub(in crate::worker) enum Completion {
    Ask(Box<job::AskJob>, ApiResult<job::Evidence>),
    WaitRead {
        attempt: InferenceAttemptId,
        key: koru_delegate_core::children::BotChildWaitEnrollmentKey,
        read: crate::worker::bots::chat::ChildWaitRead,
    },
}

impl AiWorkerClient {
    /// `ask_bot`. Answers once the ask is admitted and the wait's enrollment
    /// is durable, or with the tool code of the refusal. A call that cannot
    /// say whether the ask was recorded answers `child_status_unknown`: the
    /// model retries with the same key, never a new one.
    pub fn ask_bots_for_run(
        &self,
        mut request: RunChildAskRequest,
    ) -> ApiResult<RunChildAskPending> {
        request.deadline = request
            .deadline
            .min(Instant::now() + CHILD_ASK_BUDGET)
            .min(Instant::now() + RESPONSE_TIMEOUT);
        if request.cancellation.is_cancelled() || Instant::now() >= request.deadline {
            return Err(status_unknown());
        }
        let deadline = request.deadline;
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::Children(Incoming::Ask { request, reply }))
            .map_err(queue_failure)?;
        match response.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(value) => value,
            // The ask may have been recorded: never "nothing happened".
            Err(_) => Err(status_unknown()),
        }
    }

    /// `list_askable_bots`: ai.db only, on the writer.
    pub fn askable_bots_for_run(
        &self,
        mut request: RunAskableRequest,
    ) -> ApiResult<Vec<RunAskableBot>> {
        request.deadline = request.deadline.min(Instant::now() + RESPONSE_TIMEOUT);
        let deadline = request.deadline;
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::Children(Incoming::Askable { request, reply }))
            .map_err(queue_failure)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| {
                Err(failure(
                    "ai_worker_timeout",
                    "The Bot list did not finish within its deadline.",
                    true,
                ))
            })
    }
}

impl WorkerState {
    pub(in crate::worker) fn children(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Ask { request, reply } => self.start_child_ask(request, reply),
            Incoming::Askable { request, reply } => {
                let _ = reply.send(self.askable_bots(&request));
            }
            Incoming::Cleanup(incoming) => self.serve_child_cleanup(*incoming),
        }
    }
    pub(in crate::worker) fn children_completed(&mut self, completion: Completion) {
        match completion {
            Completion::Ask(job, result) => self.complete_child_ask(job, result),
            Completion::WaitRead { attempt, key, read } => {
                self.complete_chat_child_read(attempt, key, read)
            }
        }
    }
}

// ---- codes --------------------------------------------------------------

pub(super) fn tool_failure(code: &'static str, message: &'static str) -> Failure {
    failure(code, message, false)
}
fn with_index(mut failure: Failure, index: u8) -> Failure {
    failure
        .details
        .insert("index".into(), serde_json::Value::from(index));
    failure
}
pub(super) fn status_unknown() -> Failure {
    failure(
        codes::CHILD_STATUS_UNKNOWN,
        "The request may have been recorded. Retry with the same key; never a new one.",
        true,
    )
}
pub(super) fn invalid() -> Failure {
    failure(
        "invalid_request",
        "The request to other Bots is invalid.",
        false,
    )
}
pub(super) fn denied() -> Failure {
    failure(
        "permission_denied",
        "The current Run is not authorized to ask other Bots.",
        false,
    )
}

/// The tool failure of one stable code and, where one item is at fault, its
/// index. The texts are fixed and interpolate nothing.
pub(super) fn coded(code: &str, index: Option<u8>) -> Failure {
    let (code, message): (&'static str, &'static str) = match code {
        codes::ASK_NOT_ALLOWED => (codes::ASK_NOT_ALLOWED, "This Bot may not ask other Bots."),
        codes::RECIPIENT_UNAVAILABLE => (
            codes::RECIPIENT_UNAVAILABLE,
            "A Bot that was named cannot be asked.",
        ),
        codes::RECIPIENT_PAUSED => (codes::RECIPIENT_PAUSED, "A Bot that was named is paused."),
        codes::RECIPIENT_FULL => (
            codes::RECIPIENT_FULL,
            "A Bot that was named has too much queued work.",
        ),
        codes::RECIPIENT_FOLDER_OVERLAP => (
            codes::RECIPIENT_FOLDER_OVERLAP,
            "A Bot that was named works in a folder that overlaps this Run's.",
        ),
        codes::SELF_ASK => (codes::SELF_ASK, "A Bot cannot ask itself."),
        codes::RECIPIENT_LIMIT => (
            codes::RECIPIENT_LIMIT,
            "This Run has already asked the most Bots it may.",
        ),
        codes::WOULD_WAIT_ON_ITSELF => (
            codes::WOULD_WAIT_ON_ITSELF,
            "A Bot that was named is already waiting for this Bot.",
        ),
        codes::CHILD_CAPACITY => (
            codes::CHILD_CAPACITY,
            "This Run cannot wait for another Bot now.",
        ),
        codes::CHILD_BUDGET_LOW => (
            codes::CHILD_BUDGET_LOW,
            "This Run has too little time left to wait for another Bot.",
        ),
        codes::PAGE_UNAVAILABLE => (
            codes::PAGE_UNAVAILABLE,
            "A page that was named cannot be read by this Bot or by the Bot asked.",
        ),
        codes::ASK_KEY_CONFLICT => (
            codes::ASK_KEY_CONFLICT,
            "This key was already used with a different list.",
        ),
        codes::CHILD_WAIT_IN_PROGRESS => (
            codes::CHILD_WAIT_IN_PROGRESS,
            "This Run is already waiting for other Bots.",
        ),
        "decision_in_progress" => (
            "decision_in_progress",
            "This Run is waiting on a decision and cannot ask other Bots.",
        ),
        "invalid_request" => return invalid(),
        _ => return denied(),
    };
    let failure = tool_failure(code, message);
    match index {
        Some(index) => with_index(failure, index),
        None => failure,
    }
}

/// The tool code of an AI-side refusal.
pub(super) fn ai_refusal(refusal: BotChildAskRefusal) -> Failure {
    coded(refusal.code(), refusal.index())
}

/// What a delegate refusal of `RequestChild` means for the tool. `None`: the
/// refusal says nothing final about the ask (retryable, or an outage), and
/// the receipts stay `prepared` for the same key or the restart scan.
pub(super) fn delegate_refusal(
    error: &koru_delegate_api::Failure,
) -> Option<(&'static str, Option<u8>)> {
    let index = error
        .details
        .get("ordinal")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u8::try_from(n).ok());
    if error.retryable {
        return None;
    }
    Some(match error.code.as_str() {
        "bot_child_decision_open" => ("decision_in_progress", None),
        "bot_child_wait_open" => (codes::CHILD_WAIT_IN_PROGRESS, None),
        "bot_child_depth" => (codes::ASK_NOT_ALLOWED, None),
        "bot_child_self_ask" => (codes::SELF_ASK, index),
        "bot_child_recipient_limit" => (codes::RECIPIENT_LIMIT, None),
        "bot_child_cycle" => (codes::WOULD_WAIT_ON_ITSELF, index),
        "bot_child_recipient_paused" => (codes::RECIPIENT_PAUSED, index),
        // Correction W3: the recipient's configuration moved between this
        // worker's read and the ledger's. The ask is refused whole and a new
        // key may ask again; nothing was admitted.
        "bot_child_recipient_stale" => (codes::RECIPIENT_UNAVAILABLE, index),
        "bot_child_recipient_full" => (codes::RECIPIENT_FULL, index),
        "bot_child_capacity" => (codes::CHILD_CAPACITY, None),
        "bot_child_budget_low" => (codes::CHILD_BUDGET_LOW, None),
        "bot_child_folder_overlap" => (codes::RECIPIENT_FOLDER_OVERLAP, index),
        // A grant the parent held at this worker's read and lost before the
        // ledger's: the recipient cannot be asked with it.
        "bot_child_connections" => (codes::RECIPIENT_UNAVAILABLE, index),
        "bot_folder_unavailable" => (codes::RECIPIENT_UNAVAILABLE, index),
        "bot_conflict" => (codes::ASK_KEY_CONFLICT, None),
        "bot_invalid" => ("invalid_request", None),
        // The parent's turn is no longer the executing one (section 9 row 3),
        // or the Bot is not this owner's: nothing was admitted.
        _ => ("permission_denied", None),
    })
}

/// The answer of an ask whose rows are all settled: admitted, or refused
/// with the stored code. `None` while any row is still `prepared`.
pub(super) fn settled_answer(rows: &[BotChildChat]) -> Option<ApiResult<RunChildAskPending>> {
    if rows.is_empty() {
        return None;
    }
    if rows.iter().all(BotChildChat::admitted) {
        return Some(Ok(RunChildAskPending {
            wait: rows[0].wait,
            requests: rows
                .iter()
                .map(|row| (row.child_request, row.recipient_bot))
                .collect(),
        }));
    }
    if rows
        .iter()
        .all(|row| row.stage == koru_ai_core::bots::BotReceiptStage::Rejected)
    {
        let code = rows[0].refusal.as_deref().unwrap_or("permission_denied");
        return Some(Err(coded(code, rows[0].refusal_ordinal)));
    }
    None
}

// ---- the pages suffix -----------------------------------------------------

/// The child's first input: the task, then the pages it names.
pub(super) fn child_text(task: &str, documents: &[koru_ai_core::DocumentId]) -> String {
    let mut text = String::with_capacity(task.len() + 64 + documents.len() * 40);
    text.push_str(task);
    text.push_str(PAGES_MARKER);
    text.push_str(&documents.len().to_string());
    for document in documents {
        text.push_str("\n- ");
        text.push_str(&document.to_string());
    }
    text
}

/// The pages a stored child objective names, from this worker's own suffix
/// (the last one). `None` when the suffix is not there or malformed.
pub(super) fn child_documents(text: &str) -> Option<Vec<koru_ai_core::DocumentId>> {
    let at = text.rfind(PAGES_MARKER)?;
    let rest = &text[at + PAGES_MARKER.len()..];
    let mut lines = rest.split('\n');
    let count: usize = lines.next()?.parse().ok()?;
    let documents = lines
        .map(|line| koru_ai_core::DocumentId::parse(line.strip_prefix("- ")?))
        .collect::<Option<Vec<koru_ai_core::DocumentId>>>()?;
    (documents.len() == count && count <= koru_delegate_core::children::CHILD_DOCUMENT_LIMIT)
        .then_some(documents)
}

/// The child conversation's title: the first line of the task, bounded.
pub(super) fn child_title(task: &str) -> String {
    let first = task
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut title: String = first.trim().chars().take(80).collect();
    if title.is_empty() {
        title = koru_ai_core::NEW_CHAT_TITLE.to_owned();
    }
    title
}

/// The delegate port and its use, shared by the ask and the resend.
pub(super) fn port_unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure {
        code: "bot_unavailable".into(),
        message: "Bot coordination is unavailable.".into(),
        retryable: true,
        details: Default::default(),
    }
}
pub(super) type Port = Arc<dyn BotDelegatePort>;
pub(super) type Reply = Result<BotReply, koru_delegate_api::Failure>;
