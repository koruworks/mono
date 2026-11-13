// SPDX-License-Identifier: MPL-2.0

//! Trusted in-process Memory transport; the shared exposure gate remains closed.
use crate::worker::*;
use koru_ai_core::Cancellation;
use koru_ai_core::bots::{BotMemoryDetailPage, BotMemoryListPage, MemoryReadOperation};
use koru_delegate_core::{AttemptGeneration, BotProvenance, CapturedBotPolicy, RunId};

mod commands;
mod cursor;
pub(in crate::worker) mod read;
pub(in crate::worker) mod recovery;
pub(in crate::worker) mod write;
pub use write::{
    MemoryBaseEvidence, MemoryBaseSelection, MemoryEditBaseRequest, MemoryWriteAuthorityEvidence,
    MemoryWriteAuthorityRequest, MemoryWriteSubjectEvidence, RunMemoryEnsureRequest,
    RunMemoryEnsureResult, RunMemoryWriteSubjectRequest, memory_write_fingerprint,
};

/// A trusted composition request, never an HTTP/provider-owned principal.
/// The read job independently verifies its canonical association,
/// current Store authority and delegate generation before and after reading.
#[derive(Clone)]
pub struct RunMemoryReadRequest {
    pub run: RunId,
    pub generation: AttemptGeneration,
    pub provenance: BotProvenance,
    pub captured: CapturedBotPolicy,
    pub cancellation: Cancellation,
    pub operation: MemoryReadOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunMemoryReadResult {
    List(BotMemoryListPage),
    Detail(BotMemoryDetailPage),
}

impl AiWorkerClient {
    /// Trusted-host interface, deliberately absent from AiServerPort/HTTP.
    /// The closed gate refuses before queuing or contacting the Store/delegate.
    pub fn read_bot_memory(&self, request: RunMemoryReadRequest) -> ApiResult<RunMemoryReadResult> {
        if !koru_ai_core::bots::BOT_MEMORY_AVAILABLE {
            return Err(failure(
                "unsupported_capability",
                "Bot Memory is not available yet.",
                false,
            ));
        }
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::RunMemoryRead {
                request,
                reply,
                deadline: Instant::now() + RESPONSE_TIMEOUT,
            })
            .map_err(queue_failure)?;
        receive_api(response)
    }
}

#[cfg(test)]
pub(in crate::worker::bots) mod composition;
#[cfg(test)]
mod tests;
