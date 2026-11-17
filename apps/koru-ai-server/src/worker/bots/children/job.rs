// SPDX-License-Identifier: MPL-2.0
//! The finite off-writer work of one ask: the page checks against the
//! person's authority, and `RequestChild` to the delegate. Holds no writer
//! state and writes nothing; its result goes back to the writer.
use super::super::library::LibrarySources;
use super::*;
use koru_delegate_api::bots::BotCommand as DelegateCommand;
use koru_delegate_core::children::ChildRequest;

/// One phase of an ask.
pub(in crate::worker) enum Step {
    /// The pages the request names, each readable by this Bot and by the
    /// recipient at the ask (contract 3.3). Both Bots share the home
    /// workspace and Bots have no page-level read rule, so one check of each
    /// page under the person's authority in that workspace is both checks.
    ///
    /// Then each recipient's own Library (O160: an asked Bot runs as
    /// itself), captured exactly as a task in its own conversation captures
    /// it: its Role, Knowledge and the named pages at exact history versions,
    /// and its own Memory. `libraries` is one per item, in order, read on the
    /// writer from the recipient's current rows.
    Pages {
        owner: BotOwner,
        home: WorkspaceId,
        profile: koru_ai_core::ProfileId,
        pages: Vec<(u8, koru_ai_core::DocumentId)>,
        libraries: Vec<LibrarySources>,
    },
    /// On the writer only: prepare (or replay) the whole ask.
    Prepare,
    /// The delegate: the stored ask, sent as `RequestChild`. First each
    /// child's captured Memory index is read back for its first input, as the
    /// context job of a reply in its own conversation reads it.
    Send {
        owner: koru_delegate_core::BotOwner,
        request: Box<ChildRequest>,
        /// One per item, in order: `None` where no Memory was captured.
        memory: Vec<Option<ChildMemory>>,
    },
}

/// One recipient's Library as the capture step took it: the sources the
/// writer read, with the Memory proof filled in, and the exact history pins.
pub(in crate::worker) struct ChildLibrary {
    pub(super) sources: LibrarySources,
    pub(super) pins: Vec<koru_delegate_core::RunContextItem>,
}

/// What reading a child's captured Memory index back needs: the stored
/// capture, the recipient's binding, and the reader of its home workspace.
pub(in crate::worker) struct ChildMemory {
    pub(super) capture: BotLibraryCapture,
    pub(super) binding: Option<BotMemoryBinding>,
    pub(super) home: WorkspaceId,
    pub(super) reader: koru_ai_core::ProfileId,
}

/// A child's Memory index for its first input.
#[derive(Clone)]
pub(in crate::worker) enum ChildIndex {
    /// No Memory was captured: the first input is the request as sent.
    Absent,
    /// The captured index, its digest checked against the capture.
    Ready(Vec<u8>),
    /// The captured index cannot be read back. The ask is not sent; should
    /// a child still be started without one, its reply ends instead.
    Unavailable,
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
    /// Each recipient's captured Library, once the capture step ran. `None`
    /// for a key this Run already used (answered from its rows) and a resend.
    pub(super) libraries: Option<Vec<ChildLibrary>>,
}

pub(in crate::worker) enum Evidence {
    Pages(Vec<ChildLibrary>),
    /// The delegate's answer and each child's Memory index, in order.
    Sent(Reply, Vec<ChildIndex>),
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
                libraries,
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
                let mut captured = Vec::with_capacity(libraries.len());
                for (index, sources) in libraries.iter().enumerate() {
                    captured.push(self.capture_library(
                        *owner,
                        *home,
                        *profile,
                        &proof,
                        index as u8,
                        sources.clone(),
                    )?);
                }
                Ok(Evidence::Pages(captured))
            }
            Step::Prepare => Err(unavailable_failure()),
            Step::Send {
                owner,
                request,
                memory,
            } => {
                // Read back before the delegate hears of the ask: a child
                // whose captured Memory cannot be read must not be admitted
                // to run without it. Nothing changes anywhere on a failure:
                // the receipts stay `prepared` and the answer is
                // `child_status_unknown` (an earlier send whose reply was
                // lost may have been admitted; only the delegate knows).
                let mut indexes = Vec::with_capacity(memory.len());
                for memory in memory {
                    match self.read_index(memory.as_ref()) {
                        ChildIndex::Unavailable => return Err(status_unknown()),
                        index => indexes.push(index),
                    }
                }
                self.live()?;
                Ok(Evidence::Sent(self.send(*owner, request), indexes))
            }
        }
    }

    /// The cancellation a Store read of this job honors.
    fn cancellation(&self) -> Cancellation {
        match &self.caller {
            Caller::Tool { request, .. } => request.cancellation.clone(),
            Caller::Resend { .. } => Cancellation::default(),
        }
    }

    /// One recipient's Library, through the same two Store captures a task
    /// in its own conversation takes (`preparation.rs`): the exact history of
    /// its Role, Knowledge and the named pages, and its Memory when one is
    /// bound. A capture that fails refuses the whole ask: a child never runs
    /// with part of itself.
    fn capture_library(
        &self,
        owner: BotOwner,
        home: WorkspaceId,
        profile: koru_ai_core::ProfileId,
        proof: &koru_ai_core::bots::BotAuthoritySnapshot,
        index: u8,
        mut sources: LibrarySources,
    ) -> ApiResult<ChildLibrary> {
        // Retryable only when the Store could not answer; anything else says
        // the recipient cannot run as itself now (its Role page is gone, say).
        let refused = |retryable: bool| {
            if retryable {
                unavailable_failure()
            } else {
                coded(codes::RECIPIENT_UNAVAILABLE, Some(index))
            }
        };
        let pins = self
            .authority
            .capture(owner, home, profile, &sources.pages())
            .map_err(|error| refused(matches!(error, BotError::Unavailable | BotError::Storage)))?;
        self.live()?;
        if let Some(binding) = &sources.memory_binding {
            let reader = proof
                .workspace(binding.page.workspace)
                .map_err(|_| denied())?;
            let memory = self
                .authority
                .capture_memory(binding.owner, reader, binding, &self.cancellation())
                .map_err(|error| {
                    refused(matches!(
                        error,
                        MemoryAuthorityError::Authority(BotError::Unavailable | BotError::Storage)
                    ))
                })?;
            memory
                .validate_for_binding(binding)
                .map_err(|_| refused(false))?;
            sources.memory_capture = Some(memory);
            self.live()?;
        }
        Ok(ChildLibrary { sources, pins })
    }

    /// A child's captured Memory index, read back exactly as the context job
    /// of a reply in its own conversation reads it, and checked against the
    /// capture's digest. Never logged; it goes only into the child's input.
    fn read_index(&self, memory: Option<&ChildMemory>) -> ChildIndex {
        let Some(memory) = memory else {
            return ChildIndex::Absent;
        };
        match self.authority.replay_memory_index(
            memory.capture.owner,
            memory.reader,
            memory.home,
            &memory.capture,
            memory.binding.as_ref(),
            &self.cancellation(),
        ) {
            Ok(index)
                if index.capture == memory.capture
                    && index.validate_shape(memory.home).is_ok()
                    && *blake3::hash(&index.bytes).as_bytes() == memory.capture.index_digest =>
            {
                ChildIndex::Ready(index.bytes)
            }
            _ => ChildIndex::Unavailable,
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
