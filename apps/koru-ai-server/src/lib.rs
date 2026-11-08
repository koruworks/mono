// SPDX-License-Identifier: MPL-2.0

//! Hostable Koru AI server boundary.
//!
//! The bounded worker and optional standalone transport are implemented in
//! Workstream 1C after the persistence adapter is real.

use std::path::PathBuf;

mod attachments;
mod chat_tools;
mod store_context;
mod store_tools;
mod worker;

pub use chat_tools::{AiToolDispatcher, ChatToolAdapter};
pub use store_context::StoreContextAdapter;
pub use store_tools::StoreToolAdapter;
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
