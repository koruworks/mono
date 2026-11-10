// SPDX-License-Identifier: MPL-2.0

//! Provider-neutral Koru AI application model.
//!
//! This crate owns chat identities, state transitions, provider-neutral
//! inference values, and the ports used by the server. It deliberately knows
//! no HTTP, SQLite, OS credential API, provider SDK, window, or worker host.

pub mod agent;
pub mod attachments;
pub mod blocks;
pub mod chat_tools;
pub mod conversion;
pub mod ids;
pub mod inference;
pub mod model;
pub mod ports;
pub mod profiles;
pub mod prompt_links;
pub mod provider;
pub mod questions;
pub mod reading_order;

pub use agent::*;
pub use attachments::{
    AttachmentInput, AttachmentKind, AttachmentRecord, AttachmentSource, ChatAttachment,
    WorkspaceFileOrigin,
};
pub use blocks::{BlockCut, cut_markdown, finalize_markdown};
pub use chat_tools::*;
pub use conversion::{ConvertedChat, ConvertedMessage};
pub use ids::{
    AgentNoticeId, ChatAttachmentId, ChatId, DocumentExportId, FeedbackEventId, InferenceAttemptId,
    MessageBlockId, MessageId, ToolInvocationId,
};
pub use inference::{
    AiError, AiErrorClass, AiEvent, AiRequest, AiRequestMessage, AiRequestOptions, AiToolCall,
    AiToolDefinition, AiToolLoop, AiToolLoopLimits, AiToolResult, AiToolSafety, AiToolTurn,
    AiUsage, Cancellation, ProviderStream, RequestMeasurements, UsageSource,
};
pub use model::*;
pub use ports::{
    AiPersistence, AiProvider, AiToolExecutionContext, AiToolExecutor, StoreContextPort,
    WorkspaceFileData,
};
pub use profiles::{ChatPlacementEvidence, ChatRead, ProfileCatalog, place_chat};
pub use prompt_links::render_prompt_text;
pub use provider::*;
pub use questions::{
    AgentAnswer, AgentAnswerValue, AgentQuestion, AgentQuestionOption, AnswersRecord,
    NextMessageAnswers, QuestionsRecord,
};
pub use reading_order::{read_through, reading_order};

pub use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};
pub use koru_store_core::model::Timestamp;
