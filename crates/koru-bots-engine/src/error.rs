// SPDX-License-Identifier: MPL-2.0

/// Stable, content-free failures. Underlying SQLite and IPC messages can
/// contain values or paths, so they never become a public error or Debug.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    SchemaTooNew,
    StorageUnavailable,
    InvalidRequest,
    Unauthenticated,
    EngineUnavailable,
    RootEngineOwned,
    EngineDisabled,
    Conflict,
    RevisionConflict,
    TeamRevisionConflict,
    FolderFrozen,
    Expired,
    StaleTurn,
    NotAllowed,
    InputTooSmall,
    WorkerReplaced,
    CredentialRevoked,
    SessionConflict,
    TurnRunning,
    NoTurn,
    BindingConflict,
    OperationConflict,
    LeaseMismatch,
    OutcomeConflict,
    RecoveryPending,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SchemaTooNew => "schema_too_new",
            Self::StorageUnavailable => "storage_unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::EngineUnavailable => "engine_unavailable",
            Self::RootEngineOwned => "root_engine_owned",
            Self::EngineDisabled => "engine_disabled",
            Self::Conflict => "conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::TeamRevisionConflict => "team_revision_conflict",
            Self::FolderFrozen => "folder_frozen",
            Self::Expired => "expired",
            Self::StaleTurn => "stale_turn",
            Self::NotAllowed => "not_allowed",
            Self::InputTooSmall => "input_too_small",
            Self::WorkerReplaced => "worker_replaced",
            Self::CredentialRevoked => "credential_revoked",
            Self::SessionConflict => "session_conflict",
            Self::TurnRunning => "turn_running",
            Self::NoTurn => "no_turn",
            Self::BindingConflict => "binding_conflict",
            Self::OperationConflict => "operation_conflict",
            Self::LeaseMismatch => "lease_mismatch",
            Self::OutcomeConflict => "outcome_conflict",
            Self::RecoveryPending => "recovery_pending",
        })
    }
}

impl std::error::Error for Error {}

impl From<koru_sqlite::OpenError> for Error {
    fn from(error: koru_sqlite::OpenError) -> Self {
        match error {
            koru_sqlite::OpenError::SchemaTooNew { .. } => Self::SchemaTooNew,
            koru_sqlite::OpenError::Storage { .. } => Self::StorageUnavailable,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::StorageUnavailable
    }
}
