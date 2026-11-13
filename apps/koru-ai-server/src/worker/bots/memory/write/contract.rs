// SPDX-License-Identifier: MPL-2.0
//! Trusted in-process contracts, deliberately absent from public API schemas.
use super::*;
use serde::Serialize;

#[derive(Clone)]
pub struct RunMemoryWriteSubjectRequest {
    pub run: RunId,
    pub generation: AttemptGeneration,
    pub provenance: BotProvenance,
    pub captured: CapturedBotPolicy,
    pub action: MemoryEditAction,
    pub cancellation: Cancellation,
}
impl RunMemoryWriteSubjectRequest {
    pub(super) fn subject(&self) -> MemoryEditSubjectV1 {
        MemoryEditSubjectV1 {
            version: 1,
            owner: self.provenance.owner,
            bot: self.provenance.bot,
            submission: self.provenance.submission,
            captured_config_revision: self.provenance.config_revision,
            action: self.action,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryWriteSubjectEvidence {
    pub subject: MemoryEditSubjectV1,
    pub profile: koru_ai_core::ProfileId,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub new_work_barrier: Option<BotPolicyBarrier>,
}
#[derive(Clone)]
pub enum MemoryBaseSelection {
    UnallocatedHistory { version: DocumentVersionId },
    Allocated { allocation: MemoryEditAllocation },
}
#[derive(Clone)]
pub struct MemoryEditBaseRequest {
    pub subject_request: RunMemoryWriteSubjectRequest,
    pub selection: MemoryBaseSelection,
    pub operation: MemoryRecipeOperation,
}
#[derive(Clone)]
pub struct MemoryBaseEvidence {
    pub authority: MemoryEditAuthorityV1,
    pub version: DocumentVersionId,
    pub expected_revision: Vec<u8>,
    pub recipe: MemoryEditRecipe,
}
#[derive(Clone)]
pub struct RunMemoryEnsureRequest {
    pub subject_request: RunMemoryWriteSubjectRequest,
    pub allocation: MemoryEditAllocation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunMemoryEnsureResult {
    pub creation_request: BotRequestId,
    pub state: BotMemoryState,
    pub observed_updated_at_ms: i64,
    pub binding: Option<BotMemoryBinding>,
}
#[derive(Clone)]
pub struct MemoryWriteAuthorityRequest {
    pub subject_request: RunMemoryWriteSubjectRequest,
    pub resolved_allocation: MemoryEditAllocation,
}
#[derive(Clone, Debug)]
pub struct MemoryWriteAuthorityEvidence {
    pub authority: MemoryEditAuthorityV1,
    pub barrier: BotPolicyBarrier,
    pub profile: koru_ai_core::ProfileId,
}

pub(super) fn validate_operation(
    action: MemoryEditAction,
    operation: &MemoryRecipeOperation,
) -> ApiResult<()> {
    let actual = match operation {
        MemoryRecipeOperation::Remember { summary, detail } => {
            validate_memory_text(summary, detail).map_err(|_| invalid_text())?;
            MemoryEditAction::Remember
        }
        MemoryRecipeOperation::Correct {
            summary, detail, ..
        } => {
            validate_memory_text(summary, detail).map_err(|_| invalid_text())?;
            MemoryEditAction::Correct
        }
        MemoryRecipeOperation::Forget { .. } => MemoryEditAction::Forget,
    };
    if actual != action {
        return Err(bot_failure(BotError::Invalid));
    }
    Ok(())
}
fn invalid_text() -> Failure {
    failure(
        "invalid_memory_text",
        "Memory summary and detail must each be a bounded single paragraph. Use the ordinary editor for multiline content.",
        false,
    )
}

/// One canonical request identity shared by trusted gateway composition and
/// worker verification. No binding, mutable policy, profile or generated IDs.
pub fn memory_write_fingerprint(
    subject: &MemoryEditSubjectV1,
    run: RunId,
    operation_id: Uuid,
    base: MemoryBaseSelector,
    operation: &MemoryRecipeOperation,
) -> ApiResult<[u8; 32]> {
    validate_operation(subject.action, operation)?;
    if !subject.valid()
        || operation_id.get_version_num() != 4
        || operation_id.get_variant() != uuid::Variant::RFC4122
        || matches!(base, MemoryBaseSelector::OriginalSeed {})
            && subject.action != MemoryEditAction::Remember
        || matches!(base, MemoryBaseSelector::History { version } if version.as_uuid().is_nil())
    {
        return Err(bot_failure(BotError::Invalid));
    }
    #[derive(Serialize)]
    struct Body<'a> {
        subject: &'a MemoryEditSubjectV1,
        run: RunId,
        operation_id: Uuid,
        base: MemoryBaseSelector,
        fact: Option<Uuid>,
        summary: Option<&'a str>,
        detail: Option<&'a str>,
    }
    let (fact, summary, detail) = match operation {
        MemoryRecipeOperation::Remember { summary, detail } => {
            (None, Some(summary.as_str()), Some(detail.as_str()))
        }
        MemoryRecipeOperation::Correct {
            fact,
            summary,
            detail,
        } => (Some(*fact), Some(summary.as_str()), Some(detail.as_str())),
        MemoryRecipeOperation::Forget { fact } => (Some(*fact), None, None),
    };
    // Text bounds above cap this typed serialization, including JSON escaping.
    let encoded = serde_json::to_vec(&Body {
        subject,
        run,
        operation_id,
        base,
        fact,
        summary,
        detail,
    })
    .map_err(|_| bot_failure(BotError::Invalid))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"koru.bot.memory.request.v1");
    hash.update(&encoded);
    Ok(*hash.finalize().as_bytes())
}

impl AiWorkerClient {
    fn memory_write_request(
        &self,
        request: RunMemoryWriteSubjectRequest,
        work: Work,
    ) -> ApiResult<WriteValue> {
        if !BOT_MEMORY_WRITES_AVAILABLE {
            return Err(closed());
        }
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::RunMemoryWrite {
                request,
                work,
                reply,
                deadline: Instant::now() + RESPONSE_TIMEOUT,
            })
            .map_err(queue_failure)?;
        receive_api(response)
    }
    pub fn memory_write_subject(
        &self,
        request: RunMemoryWriteSubjectRequest,
    ) -> ApiResult<MemoryWriteSubjectEvidence> {
        match self.memory_write_request(request, Work::Subject)? {
            WriteValue::Subject(value) => Ok(value),
            _ => Err(unavailable_failure()),
        }
    }
    pub fn prepare_memory_edit_base(
        &self,
        request: MemoryEditBaseRequest,
    ) -> ApiResult<MemoryBaseEvidence> {
        match self.memory_write_request(
            request.subject_request,
            Work::Base {
                selection: request.selection,
                operation: request.operation,
            },
        )? {
            WriteValue::Base(value) => Ok(value),
            _ => Err(unavailable_failure()),
        }
    }
    pub fn ensure_run_memory(
        &self,
        request: RunMemoryEnsureRequest,
    ) -> ApiResult<RunMemoryEnsureResult> {
        match self.memory_write_request(
            request.subject_request,
            Work::Ensure {
                allocation: request.allocation,
            },
        )? {
            WriteValue::Ensure(value) => Ok(value),
            _ => Err(unavailable_failure()),
        }
    }
    pub fn memory_write_authority(
        &self,
        request: MemoryWriteAuthorityRequest,
    ) -> ApiResult<MemoryWriteAuthorityEvidence> {
        match self.memory_write_request(
            request.subject_request,
            Work::Authority {
                allocation: request.resolved_allocation,
            },
        )? {
            WriteValue::Authority(value) => Ok(value),
            _ => Err(unavailable_failure()),
        }
    }
}
