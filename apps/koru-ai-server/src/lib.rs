// SPDX-License-Identifier: MPL-2.0

//! Hostable Koru AI server boundary.
//!
//! The bounded worker and optional standalone transport are implemented in
//! Workstream 1C after the persistence adapter is real.

use std::path::PathBuf;

mod attachments;
mod bots;
pub use bots::BotAuthorityAdapter;
mod chat_tools;
mod store_context;
mod store_tools;
mod worker;

pub use chat_tools::{AiToolDispatcher, ChatToolAdapter};
pub use store_context::StoreContextAdapter;
pub use store_tools::{
    NativeStoreAuthority, StoreToolAdapter, native_document_title, native_store_backend,
};
pub use worker::bots::memory::{
    MemoryBaseEvidence, MemoryBaseSelection, MemoryEditBaseRequest, MemoryWriteAuthorityEvidence,
    MemoryWriteAuthorityRequest, MemoryWriteSubjectEvidence, RunMemoryEnsureRequest,
    RunMemoryEnsureResult, RunMemoryWriteSubjectRequest, memory_write_fingerprint,
};
pub use worker::bots::memory::{RunMemoryReadRequest, RunMemoryReadResult};
pub use worker::bots::policy::{NativePolicyEvidence, NativePolicyRequest};
pub use worker::native_recovery::{
    NativeMutationGate, NativeRecoveryAdmission, NativeRecoveryRefusal,
};
pub use worker::routine_call::{RoutineCall, RoutineCallContext};
pub use worker::{
    AiWorker, AiWorkerClient, AiWorkerInfo, WORKER_COMMAND_CAPACITY, WorkerConfig, WorkerError,
    WorkerExit, WorkerHandle, WorkerStatus,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub root: PathBuf,
    pub channel: String,
}

impl ServerConfig {
    pub fn new(root: PathBuf, channel: impl Into<String>) -> Self {
        Self {
            root,
            channel: channel.into(),
        }
    }

    pub fn database_path(&self) -> PathBuf {
        self.root.join(koru_ai_sqlite::DATABASE_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_root_owns_exactly_one_ai_database() {
        let config = ServerConfig::new(PathBuf::from("ai"), "internal-debug");
        assert_eq!(config.database_path(), PathBuf::from("ai").join("ai.db"));
    }
}
