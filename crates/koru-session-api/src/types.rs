// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{
    Attachment, AttachmentId, ControllerLease, ControllerLeaseToken, GridSize, IdempotencyKey,
    KeyStroke, RenderSnapshot, RowRange, ScrollbackPage, SendPendingInputResult, SessionId,
    SessionMode, SessionProfileKey, SessionSummary, StreamSequence, TerminalGeneration,
    ViewportGeneration,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmptyRequest {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateSessionRequest {
    pub idempotency_key: IdempotencyKey,
    pub profile: SessionProfileKey,
    pub parent_session_id: Option<SessionId>,
    /// Raw remains the compatibility default. Managed creation is accepted
    /// only for a profile whose provider passes the rooted host handshake.
    #[serde(default)]
    pub mode: SessionMode,
    /// An absolute local path. The Agent-owned host validates it at spawn.
    pub working_directory: String,
    pub size: GridSize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsRequest {
    pub parent_session_id: Option<SessionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachSessionRequest {
    pub viewport_generation: ViewportGeneration,
    pub columns: u16,
    pub overscan_rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DetachSessionRequest {
    pub controller_lease: ControllerLeaseToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotRequest {
    pub viewport_generation: ViewportGeneration,
    pub range: RowRange,
    pub columns: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotResponse {
    pub snapshot: RenderSnapshot,
    /// Independent cursors captured atomically with `snapshot`. After any
    /// stream gap, reconnect using both values; neither can be derived from the
    /// render snapshot's own sequence.
    pub resume: SubscribeRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScrollbackRequest {
    pub terminal_generation: TerminalGeneration,
    pub viewport_generation: ViewportGeneration,
    pub range: RowRange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScrollbackResponse {
    pub page: ScrollbackPage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WriteBytesRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    /// Base64-encoded process bytes. Access logs never record this value.
    pub data_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WriteTextRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendKeyRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    pub key: KeyStroke,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PasteTextRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InterruptRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResizeRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    pub size: GridSize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadAttachmentRequest {
    pub controller_lease: ControllerLeaseToken,
    pub attachment_id: AttachmentId,
    /// Base64-encoded image bytes. Media type and extension are detected from
    /// the decoded bytes; the public caller supplies neither a path nor a name.
    pub data_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentList {
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoveAttachmentRequest {
    pub controller_lease: ControllerLeaseToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendPendingInputRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
    pub text: String,
    /// Ordered by the caller composing this logical input.
    pub attachment_ids: Vec<AttachmentId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<koru_session_protocol::PathReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminateRequest {
    pub idempotency_key: IdempotencyKey,
    pub controller_lease: ControllerLeaseToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandReceipt {
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmptyResponse {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionReceipt {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeRequest {
    /// Last event sequence the caller consumed. Event and raw-output counters
    /// are independent and therefore cannot share one cursor.
    pub after_event: Option<StreamSequence>,
    /// Last raw-output sequence the caller consumed.
    pub after_output: Option<StreamSequence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionStreamSource {
    Event,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStreamItem {
    Event {
        event: koru_session_protocol::SequencedSessionEvent,
    },
    Output {
        chunk: koru_session_protocol::RawOutputChunk,
    },
    Gap {
        source: SessionStreamSource,
        expected: StreamSequence,
        actual: StreamSequence,
        resnapshot_required: bool,
    },
    Closed {
        error: Option<koru_session_protocol::SessionFailure>,
    },
}

// These aliases make the response types named in the catalog explicit while
// preserving the protocol DTO as the one serialized declaration.
pub type CreateSessionResponse = SessionSummary;
pub type GetSessionResponse = SessionSummary;
pub type AttachSessionResponse = ControllerLease;
pub type UploadAttachmentResponse = Attachment;
pub type RemoveAttachmentResponse = Attachment;
pub type SendPendingInputResponse = SendPendingInputResult;
