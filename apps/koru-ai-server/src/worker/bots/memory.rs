// SPDX-License-Identifier: MPL-2.0

//! Staged in-process Memory transport. No worker/client dispatch is enabled in D0.
use crate::worker::{AiWorkerClient, ApiResult, failure};
use koru_ai_core::Cancellation;
use koru_ai_core::bots::{BotMemoryDetailPage, BotMemoryListPage, MemoryReadOperation};
use koru_delegate_core::{AttemptGeneration, BotProvenance, CapturedBotPolicy, RunId};

/// A trusted composition request, never an HTTP/provider-owned principal.
/// The future read job must independently verify its canonical association,
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
    /// Staged trusted-host interface, deliberately absent from AiServerPort/HTTP.
    /// D0 never queues work or contacts the Store/delegate. D1 supplies the
    /// reviewed authority checks before the shared exposure gate can open.
    pub fn read_bot_memory(
        &self,
        _request: RunMemoryReadRequest,
    ) -> ApiResult<RunMemoryReadResult> {
        Err(failure(
            "unsupported_capability",
            "Bot Memory is not available yet.",
            false,
        ))
    }
}

#[cfg(test)]
mod tests;
