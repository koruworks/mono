// SPDX-License-Identifier: MPL-2.0

//! The dispatcher's announce (IPC 18, `AnnouncePrivateDispatcher`).
//!
//! A process that died while it owned an effect row can never record what
//! became of it. The next process says who it is, and the delegate then
//! closes every open row of the same owner that another dispatcher opened.
//! So the announce is owed **whenever the writer adopts an owner**: at start,
//! when the owner changes, and when the delegate's boot changes (which the
//! writer observes the same way; a repeated announce changes nothing).
//!
//! The writer only notes that an announce is owed. This thread sends it, on
//! the back-off the dispatcher's receipts use, until the delegate answers.
//! It never runs on the writer thread: the delegate call can wait.

use super::*;
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};

type DelegateOwner = koru_delegate_core::BotOwner;

const ANNOUNCE_BACKOFF_FIRST: Duration = Duration::from_millis(250);
const ANNOUNCE_BACKOFF_MAX: Duration = Duration::from_secs(32);
/// One announce's wait for the delegate. A late answer is not an answer: the
/// announce is sent again, and it is idempotent.
const ANNOUNCE_WITHIN: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Owed {
    /// The owner whose announce the delegate has not acknowledged, with the
    /// adoption it belongs to. A later adoption replaces it.
    owner: Option<(u64, DelegateOwner)>,
    adoptions: u64,
    acknowledged: u64,
}

/// What the writer and the announce thread share: which announce is owed.
#[derive(Default)]
pub(in crate::worker) struct AnnounceBoard {
    owed: Mutex<Owed>,
}
impl AnnounceBoard {
    fn lock(&self) -> std::sync::MutexGuard<'_, Owed> {
        self.owed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    /// The writer adopted `owner`. Never waits.
    pub(in crate::worker) fn adopted(&self, owner: DelegateOwner) {
        let mut owed = self.lock();
        owed.adoptions += 1;
        owed.owner = Some((owed.adoptions, owner));
    }
    fn next(&self) -> Option<(u64, DelegateOwner)> {
        self.lock().owner
    }
    /// The delegate acknowledged adoption `serial`. A newer adoption stays
    /// owed.
    fn acknowledge(&self, serial: u64) {
        let mut owed = self.lock();
        if owed.owner.is_some_and(|(current, _)| current == serial) {
            owed.owner = None;
        }
        owed.acknowledged += 1;
    }
    /// Adoptions noted and announces acknowledged, for tests.
    #[cfg(test)]
    pub(in crate::worker) fn counts(&self) -> (u64, u64, bool) {
        let owed = self.lock();
        (owed.adoptions, owed.acknowledged, owed.owner.is_some())
    }
}

pub(super) fn spawn(
    board: Arc<AnnounceBoard>,
    delegate: Arc<dyn BotDelegatePort>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("koru-ai-server connections announce".into())
        .spawn(move || run(&board, delegate.as_ref(), &shutdown))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// How one announce ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Announced {
    /// The delegate answered for this dispatcher.
    Acknowledged,
    /// A typed refusal that committed nothing and will not change by asking
    /// again (the private owner is not composed in the delegate, or this
    /// owner is not its device's): nothing is owed.
    Refused,
    /// No answer, or one that says nothing: owed still.
    Unanswered,
}

/// Send one announce for `owner` with this process's dispatcher id.
pub(super) fn announce(delegate: &dyn BotDelegatePort, owner: DelegateOwner) -> Announced {
    let dispatcher = koru_connections::process_dispatcher();
    let answer = catch_unwind(AssertUnwindSafe(|| {
        delegate.execute_within(
            BotCommand::AnnouncePrivateDispatcher { owner, dispatcher },
            ANNOUNCE_WITHIN,
        )
    }));
    match answer {
        Ok(Ok(BotReply::PrivateDispatcher {
            dispatcher: answered,
            resolved,
        })) if answered == dispatcher => {
            // A count only. The rows it closed are the delegate's to report.
            tracing::debug!(
                target: "koru_ai::connections",
                resolved,
                "the private dispatcher was announced"
            );
            Announced::Acknowledged
        }
        Ok(Err(failure)) if failure.code.starts_with("bot_") && !failure.retryable => {
            tracing::debug!(
                target: "koru_ai::connections",
                code = %failure.code,
                "the private dispatcher announce was refused"
            );
            Announced::Refused
        }
        _ => Announced::Unanswered,
    }
}

fn run(board: &AnnounceBoard, delegate: &dyn BotDelegatePort, shutdown: &AtomicBool) {
    let mut backoff = ANNOUNCE_BACKOFF_FIRST;
    let mut due = Instant::now();
    let mut last = None;
    while !shutdown.load(Ordering::Acquire) {
        let Some((serial, owner)) = board.next() else {
            std::thread::sleep(IDLE);
            continue;
        };
        // A new adoption is announced at once, whatever the last one's
        // back-off had reached.
        if last != Some(serial) {
            last = Some(serial);
            backoff = ANNOUNCE_BACKOFF_FIRST;
            due = Instant::now();
        }
        if Instant::now() < due {
            std::thread::sleep(IDLE.min(due.saturating_duration_since(Instant::now())));
            continue;
        }
        match announce(delegate, owner) {
            Announced::Acknowledged | Announced::Refused => board.acknowledge(serial),
            Announced::Unanswered => {
                due = Instant::now() + backoff;
                backoff = (backoff * 2).min(ANNOUNCE_BACKOFF_MAX);
            }
        }
    }
}
