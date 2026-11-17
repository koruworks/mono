// SPDX-License-Identifier: MPL-2.0
//! The finite off-writer work of one ask: the page checks against the
//! person's authority, and `RequestChild` to the delegate. Holds no writer
//! state and writes nothing; its result goes back to the writer.
use super::*;
use koru_delegate_api::bots::BotCommand as DelegateCommand;
use koru_delegate_core::children::ChildRequest;

/// One phase of an ask.
pub(in crate::worker) enum Step {
    /// The pages the request names, each readable by this Bot and by the
    /// recipient at the ask (contract 3.3). Both Bots share the home
    /// workspace and Bots have no page-level read rule, so one check of each
    /// page under the person's authority in that workspace is both checks.
    Pages {
        owner: BotOwner,
        home: WorkspaceId,
        profile: koru_ai_core::ProfileId,
        pages: Vec<(u8, koru_ai_core::DocumentId)>,
    },
    /// On the writer only: prepare (or replay) the whole ask.
    Prepare,
    /// The delegate: the stored ask, sent as `RequestChild`.
    Send {
        owner: koru_delegate_core::BotOwner,
        request: Box<ChildRequest>,
    },
}

/// Who is owed the ask's answer.
pub(in crate::worker) enum Caller {
    /// The Run tool, waiting within its 30-second call.
    Tool {
        request: RunChildAskRequest,
        reply: SyncSender<ApiResult<RunChildAskPending>>,
    },
    /// The restart scan: nobody waits. The receipts it holds in the
    /// coordination set are released when the result comes back.
    Resend { held: Vec<(BotId, BotRequestId)> },
}
impl Caller {
    pub(super) fn is_resend(&self) -> bool {
        matches!(self, Self::Resend { .. })
    }
}

pub(in crate::worker) struct AskJob {
    pub(super) step: Step,
    pub(super) caller: Caller,
    pub(super) owner: BotOwner,
    pub(super) parent_run: koru_delegate_core::RunId,
    pub(super) ask_key: uuid::Uuid,
    pub(super) authority: Arc<dyn BotAuthority>,
    pub(super) delegate: Port,
    pub(super) deadline: Instant,
}

pub(in crate::worker) enum Evidence {
    Pages,
    Sent(Reply),
}

/// Retryable refusals are asked again inside the call, after 250 ms doubling
/// to this.
const RETRY_CAP: Duration = Duration::from_secs(2);

impl AskJob {
    /// The answer to whoever waits. A resend has nobody to answer.
    pub(super) fn answer(self: Box<Self>, value: ApiResult<RunChildAskPending>) {
        if let Caller::Tool { reply, .. } = self.caller {
            let _ = reply.send(value);
        }
    }

    pub(in crate::worker) fn run(self: Box<Self>) -> Completion {
        let result = catch_unwind(AssertUnwindSafe(|| self.execute()))
            .unwrap_or_else(|_| Err(unavailable_failure()));
        Completion::Ask(self, result)
    }

    fn live(&self) -> ApiResult<()> {
        let cancelled = match &self.caller {
            Caller::Tool { request, .. } => request.cancellation.is_cancelled(),
            Caller::Resend { .. } => false,
        };
        if cancelled || Instant::now() >= self.deadline {
            return Err(match self.step {
                Step::Send { .. } => status_unknown(),
                _ => failure(
                    "ai_worker_timeout",
                    "The request to other Bots did not finish within its deadline.",
                    true,
                ),
            });
        }
        Ok(())
    }

    fn execute(&self) -> ApiResult<Evidence> {
        self.live()?;
        match &self.step {
            Step::Pages {
                owner,
                home,
                profile,
                pages,
            } => {
                let wanted: Vec<BotPage> = pages
                    .iter()
                    .map(|(_, document)| BotPage {
                        workspace: *home,
                        document: *document,
                    })
                    .collect();
                let proof = self
                    .authority
                    .inspect(&[*home], &wanted)
                    .map_err(|_| denied())?;
                self.live()?;
                if proof.owner != *owner
                    || proof.workspace(*home).map_err(|_| denied())? != *profile
                {
                    return Err(denied());
                }
                for ((index, document), page) in pages.iter().zip(&wanted) {
                    if proof.page(page).is_err() {
                        let mut failure = coded(codes::PAGE_UNAVAILABLE, Some(*index));
                        failure.details.insert(
                            "document_id".into(),
                            serde_json::Value::from(document.to_string()),
                        );
                        return Err(failure);
                    }
                }
                Ok(Evidence::Pages)
            }
            Step::Prepare => Err(unavailable_failure()),
            Step::Send { owner, request } => Ok(Evidence::Sent(self.send(*owner, request))),
        }
    }

    /// `RequestChild` under the delegate's current boot. A retryable refusal
    /// (`bot_child_wait_closing`, `scope_recovery_pending`, an outage) is
    /// asked again within the deadline; a stale boot is probed again. A
    /// resend asks once: the restart scan's own backoff repeats it.
    fn send(&self, owner: koru_delegate_core::BotOwner, request: &ChildRequest) -> Reply {
        let mut boot = None;
        let mut delay = Duration::from_millis(250);
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(port_unavailable());
            }
            let current = match boot {
                Some(boot) => boot,
                None => match crate::native_effect::probe_boot(self.delegate.as_ref(), owner) {
                    Some(found) => {
                        boot = Some(found);
                        found
                    }
                    None => {
                        if self.caller.is_resend() || !self.pause(&mut delay) {
                            return Err(port_unavailable());
                        }
                        continue;
                    }
                },
            };
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(port_unavailable());
            }
            match self.delegate.execute_within(
                DelegateCommand::RequestChild {
                    request: request.clone(),
                    boot: current,
                },
                remaining,
            ) {
                Ok(reply) => return Ok(reply),
                Err(error) if error.code == "bot_boot_stale" => {
                    boot = None;
                    if self.caller.is_resend() || !self.pause(&mut delay) {
                        return Err(port_unavailable());
                    }
                }
                Err(error) if error.retryable => {
                    if self.caller.is_resend() || !self.pause(&mut delay) {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Wait before asking again, within the deadline. `false` when no time
    /// is left for another try.
    fn pause(&self, delay: &mut Duration) -> bool {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining <= *delay {
            return false;
        }
        std::thread::sleep(*delay);
        *delay = (*delay * 2).min(RETRY_CAP);
        true
    }
}
