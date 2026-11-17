// SPDX-License-Identifier: MPL-2.0

//! Delegate IPC 19: child requests (M6). One Bot Run asks other Bots and
//! waits: the ask, its read, and the read of the wait's cleanup intent.
//!
//! All three commands and their replies ride the authenticated private Bot
//! channel. None is a public frame or a model tool. Every value outside the
//! child submissions of `RequestChild` is an identifier, a digest, a closed
//! token, a revision, a number or a time: no task text, title, path or page
//! content can be spelled in a read or in any reply. The neutral types live
//! in `koru-delegate-core` (the ledger needs them); this module re-exports
//! them and adds the encoded bounds and the fixed failure texts of the new
//! refusal codes.
pub use koru_delegate_core::children::{
    BOT_CHILD_RESUME_FLOOR_MS, BOT_CHILD_WAIT_MS, BotChildWaitCleanupIntentV1,
    BotChildWaitEnrollmentKey, BotChildWaitEnrollmentReply, BotChildWaitObservation,
    CHILD_DOCUMENT_LIMIT, CHILD_REQUEST_LIMIT, ChildClock, ChildPolicyRef, ChildRefusal,
    ChildRefused, ChildRequest, ChildRequestBindingV1, ChildRequestItem, ChildRequestReceipt,
    ChildRequestState, ChildRequested, ChildRequests, ChildResume, ChildSettledBy,
    ChildWaitDisposition, ChildWaitView, EnrollBotChildWaitCleanup, ReadBotChildWaitCleanup,
};
use koru_delegate_core::decisions::DecisionValidate;
pub use koru_delegate_core::{ChildParent, ChildRequestId};
use serde::{Deserialize, Serialize};

/// The encoded bound of the two read commands and of the `child_requested`
/// and `child_requests` replies. `RequestChild` carries up to three child
/// submissions and keeps the existing frame bound.
pub const CHILD_WIRE_LIMIT: usize = 8 * 1024;

/// The encoded bound of the `child_wait` reply. It carries two retained
/// evidence records (the origin stop and the consumer cleanup), each bounded
/// at 4 KiB by its own type, so it cannot share the 8 KiB bound.
pub const CHILD_WAIT_WIRE_LIMIT: usize = 16 * 1024;

/// The largest request frame body that may carry one of the two bounded read
/// commands: the 8 KiB command, the frame tag and the bootstrap authority at
/// its longest escaped spelling.
pub const CHILD_FRAME_LIMIT: usize = CHILD_WIRE_LIMIT + 2048;

/// Whether `RequestChild` created the rows now or found them retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildRequestedOutcome {
    Created,
    AlreadyCreated,
}

pub(super) fn within<T: Serialize + ?Sized>(value: &T, limit: usize) -> bool {
    // Counted while encoding: nothing over the bound is ever buffered.
    struct Bounded(usize);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bounded(limit), value).is_ok()
}

fn v4(id: uuid::Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

/// Shape of an ask as it may be sent: the core's own check (one to three
/// items in ordinal order, one wait, one parent, one person, each binding
/// matching its submission). The ledger still decides everything else.
pub(super) fn request_valid(request: &ChildRequest) -> bool {
    request.validate().is_ok()
}

pub(super) fn subject_valid(key: &koru_delegate_core::BotRunKey) -> bool {
    v4(key.bot.as_uuid()) && v4(key.run.as_uuid())
}

pub(super) fn wait_subject_valid(
    bot: koru_delegate_core::BotId,
    run: koru_delegate_core::RunId,
    wait: koru_delegate_core::decisions::BotWaitId,
) -> bool {
    v4(bot.as_uuid()) && v4(run.as_uuid()) && v4(wait.as_uuid())
}

fn receipts_valid(receipts: &[ChildRequestReceipt]) -> bool {
    receipts.len() <= CHILD_REQUEST_LIMIT
        && receipts.iter().all(|receipt| receipt.validate().is_ok())
        && receipts
            .iter()
            .all(|receipt| receipt.parent == receipts[0].parent)
}

/// A `child_requested` reply the ledger could have produced: one to three
/// receipts in ordinal order, all of the wait the reply names, and the frozen
/// intent of that same wait and parent.
pub(super) fn requested_valid(
    wait: &ChildWaitView,
    enrollment: &BotChildWaitCleanupIntentV1,
    receipts: &[ChildRequestReceipt],
) -> bool {
    !receipts.is_empty()
        && receipts_valid(receipts)
        && enrollment.validate().is_ok()
        && enrollment.wait_id == wait.wait
        && usize::from(wait.open_requests) <= receipts.len()
        && receipts.iter().enumerate().all(|(index, receipt)| {
            usize::from(receipt.ordinal) == index
                && receipt.wait == wait.wait
                && receipt.parent.owner == enrollment.owner
                && receipt.parent.bot == enrollment.bot
                && receipt.parent.run == enrollment.run
        })
}

/// A `child_requests` reply: at most three receipts of one parent Run, and an
/// open wait that counts no more open requests than there are receipts.
pub(super) fn requests_valid(
    wait: Option<&ChildWaitView>,
    requests: &[ChildRequestReceipt],
) -> bool {
    receipts_valid(requests)
        && wait.is_none_or(|wait| usize::from(wait.open_requests) <= requests.len())
}

pub(super) fn observation_valid(observation: &BotChildWaitObservation) -> bool {
    observation.validate().is_ok()
}

/// Whether a raw request body names a bounded child read and is over its
/// bound. Runs before the frame is decoded, like
/// [`super::private_frame_oversized`]: the probe borrows the one tag it reads
/// and skips everything else, so nothing oversized is allocated.
pub fn child_frame_oversized(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Frame<'a> {
        #[serde(borrow, default)]
        command: Option<Command<'a>>,
    }
    #[derive(Deserialize)]
    struct Command<'a> {
        #[serde(borrow, default)]
        command: Option<&'a str>,
    }
    body.len() > CHILD_FRAME_LIMIT
        && serde_json::from_slice::<Frame<'_>>(body).is_ok_and(|frame| {
            matches!(
                frame.command.and_then(|command| command.command),
                Some("read_child_requests" | "read_bot_child_wait")
            )
        })
}

/// The one code a reader of a child wait can be refused with beyond the
/// existing ones: no such wait for that Run.
pub const CHILD_WAIT_UNKNOWN: &str = "bot_child_wait_unknown";

/// The refusal codes IPC 19 adds, each with its one fixed English text and
/// whether the same caller may retry. No text interpolates a value. Each
/// means the command committed nothing. Exactly one is retryable: a finished
/// wait that still owes its last publication.
pub const CHILD_FAILURES: &[(&str, &str, bool)] = &[
    (
        "bot_child_decision_open",
        "This Run is waiting for a decision and cannot ask another Bot.",
        false,
    ),
    (
        "bot_child_wait_open",
        "This Run is already waiting for another Bot.",
        false,
    ),
    (
        "bot_child_wait_closing",
        "This Run's earlier wait is still closing.",
        true,
    ),
    (
        "bot_child_depth",
        "A Run that was asked by a Bot cannot ask another Bot.",
        false,
    ),
    ("bot_child_self_ask", "A Bot cannot ask itself.", false),
    (
        "bot_child_recipient_limit",
        "This Run has already asked the most Bots it may.",
        false,
    ),
    (
        "bot_child_cycle",
        "The Bot that was asked is already waiting for this Bot.",
        false,
    ),
    (
        "bot_child_recipient_paused",
        "The Bot that was asked is paused.",
        false,
    ),
    (
        "bot_child_recipient_stale",
        "The request was built for an earlier configuration of the Bot that was asked.",
        false,
    ),
    (
        "bot_child_recipient_full",
        "The Bot that was asked has too much queued work.",
        false,
    ),
    (
        "bot_child_capacity",
        "This Run cannot wait for another Bot now.",
        false,
    ),
    (
        "bot_child_budget_low",
        "This Run has too little time left to wait for another Bot.",
        false,
    ),
    (
        "bot_child_folder_overlap",
        "The Bot that was asked works in a folder that overlaps this Run's.",
        false,
    ),
    (
        "bot_child_connections",
        "The request would give the Bot that was asked a connection this Run does not have.",
        false,
    ),
    (
        CHILD_WAIT_UNKNOWN,
        "This Run has no such wait for another Bot.",
        false,
    ),
];

/// The fixed failure of one of the new codes; `None` for every other code.
pub fn child_failure(code: &str) -> Option<crate::Failure> {
    CHILD_FAILURES
        .iter()
        .find(|(known, _, _)| *known == code)
        .map(|(known, message, retryable)| crate::Failure::new(*known, *message, *retryable))
}

/// The failure of a refused ask: its fixed text and, for a per-recipient
/// code, the item's `ordinal` in `details`.
pub fn child_refused(refused: ChildRefused) -> crate::Failure {
    let failure = child_failure(refused.code.code()).unwrap_or_else(|| {
        crate::Failure::new(
            refused.code.code(),
            "The Bot operation was refused.",
            refused.code.retryable(),
        )
    });
    match refused.ordinal {
        Some(ordinal) => failure.with_detail("ordinal", u64::from(ordinal)),
        None => failure,
    }
}
