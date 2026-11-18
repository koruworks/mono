// SPDX-License-Identifier: MPL-2.0
//! The person's engine carrier. Authentication stays at the gateway; current
//! Store owner/page checks happen off the HTTP reactor and engine actor.
use crate::{GatewayPrincipal, GatewayState, bots_workspace::Engine};
use axum::{
    Json,
    extract::{Extension, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use koru_ai_core::bots::{BotAuthority, BotError, BotPage};
use koru_bots_engine::{
    Error,
    person::{self, Change, Command, Query},
    worker,
};
use serde_json::Value;
use std::sync::Arc;

type Call = Arc<dyn Fn(person::Request) -> Result<person::Reply, Error> + Send + Sync>;
type CreationCall = Arc<
    dyn Fn(koru_bots_engine::creation::Request) -> Result<koru_bots_engine::creation::Reply, Error>
        + Send
        + Sync,
>;
mod creation;
mod window;
pub(crate) struct Gateway {
    authority: Arc<dyn BotAuthority>,
    call: Call,
    creation: CreationCall,
    permits: Arc<tokio::sync::Semaphore>,
    root: std::path::PathBuf,
    folders: Arc<dyn koru_bots_engine::folders::Folders + Send + Sync>,
}
impl GatewayState {
    /// Only the supervised host installs this capability. It is separate from
    /// a public bearer and never passed to a Bot worker or coding provider.
    pub fn set_bots_engine(&self, engine: Engine, device: &str, root: std::path::PathBuf) -> bool {
        let Some(authority) =
            koru_ai_server::BotAuthorityAdapter::from_device(self.store.client(), device)
        else {
            return false;
        };
        let creation_engine = engine.clone();
        self.bots_engine
            .set(Arc::new(Gateway {
                root,
                folders: Arc::new(koru_bots_engine::folders::Native),
                authority: Arc::new(authority),
                creation: Arc::new(move |request| {
                    match worker::request(
                        &creation_engine.endpoint,
                        creation_engine.authority.expose(),
                        worker::Command::Creation(request),
                    )? {
                        worker::Reply::Creation(reply) => Ok(reply),
                        worker::Reply::Refused(error) => Err(error),
                        _ => Err(Error::EngineUnavailable),
                    }
                }),
                call: Arc::new(move |request| {
                    match worker::request(
                        &engine.endpoint,
                        engine.authority.expose(),
                        worker::Command::Person(request),
                    )? {
                        worker::Reply::Person(reply) => Ok(reply),
                        worker::Reply::Refused(error) => Err(error),
                        _ => Err(Error::EngineUnavailable),
                    }
                }),
                permits: Arc::new(tokio::sync::Semaphore::new(8)),
            }))
            .is_ok()
    }
}
fn authority_error(error: BotError) -> Error {
    match error {
        BotError::Forbidden | BotError::NotFound => Error::NotAllowed,
        BotError::Invalid => Error::InvalidRequest,
        _ => Error::StorageUnavailable,
    }
}
fn permitted(principal: &GatewayPrincipal) -> bool {
    // Management is granted only to an Agent-vouched first-party session.
    // The narrow external task facade has separate read/run projections.
    principal
        .scopes
        .iter()
        .any(|scope| scope == koru_ai_api::AiScope::ManageBots.as_str())
}
impl Gateway {
    fn configuration(
        &self,
        configuration: &koru_bots_engine::configuration::Configuration,
        avatar: Option<u8>,
    ) -> Result<(), Error> {
        koru_ai_core::bots::validate_avatar(avatar).map_err(authority_error)?;
        if let Some(path) = &configuration.working_folder {
            let path = std::path::Path::new(path);
            let bound = koru_bots_engine::folders::selected(
                self.folders.as_ref(),
                path,
                std::slice::from_ref(&self.root),
            )
            .map_err(|error| match error {
                koru_bots_engine::folders::Failure::Unavailable => Error::StorageUnavailable,
                koru_bots_engine::folders::Failure::Reserved => Error::NotAllowed,
                _ => Error::InvalidRequest,
            })?;
            // Store a binding, not an alias whose target can change later.
            // The form's folder preparation supplies this canonical spelling.
            if bound != path {
                return Err(Error::InvalidRequest);
            }
        }
        Ok(())
    }
    fn execute(&self, principal: &GatewayPrincipal, command: Command) -> Result<Value, Error> {
        if !permitted(principal) {
            return Err(Error::NotAllowed);
        }
        let owner = self.authority.owner().map_err(authority_error)?;
        self.execute_owner(owner, command)
    }
    /// Only called after first-party authentication and current owner lookup.
    fn execute_owner(
        &self,
        owner: koru_ai_core::bots::BotOwner,
        command: Command,
    ) -> Result<Value, Error> {
        let scope = owner.user_scope.to_storage();
        let send = |command| {
            (self.call)(person::Request {
                owner: scope.clone(),
                sender: format!("person:{scope}"),
                command,
            })
            .map(|reply| reply.0)
        };
        if let Command::Change { id, action } = &command {
            let receipt = send(Command::Read {
                query: Query::ChangeReceipt {
                    id: *id,
                    action: action.clone(),
                },
            })?;
            if !receipt.is_null() {
                return Ok(receipt);
            }
        }
        let read_bot = |bot| -> Result<person::Bot, Error> {
            serde_json::from_value(send(Command::Read {
                query: Query::Bot { bot },
            })?)
            .map_err(|_| Error::EngineUnavailable)
        };
        let mut workspaces = vec![];
        let mut pages = vec![];
        match &command {
            Command::Change {
                action: Change::Create { definition, .. },
                ..
            } => {
                self.configuration(&definition.configuration, definition.avatar)?;
                workspaces.push(definition.access.workspace);
                pages.extend(
                    definition
                        .role
                        .iter()
                        .chain(&definition.knowledge)
                        .chain(definition.memory.iter())
                        .map(|id| BotPage {
                            workspace: definition.access.workspace,
                            document: koru_ai_core::DocumentId::from_uuid(*id),
                        }),
                );
                pages.extend(
                    definition
                        .access
                        .selected_pages
                        .iter()
                        .map(|document| BotPage {
                            workspace: definition.access.workspace,
                            document: *document,
                        }),
                );
            }
            Command::Change {
                action:
                    Change::Configure {
                        bot,
                        configuration,
                        avatar,
                        role,
                        ..
                    },
                ..
            } => {
                let current = read_bot(*bot)?;
                koru_ai_core::bots::validate_avatar(*avatar).map_err(authority_error)?;
                // A missing existing folder must not prevent fixing a name or
                // model. Launch revalidates the binding before native work.
                if current.definition.configuration.working_folder != configuration.working_folder {
                    self.configuration(configuration, *avatar)?;
                }
                let home = current.definition.access.workspace;
                workspaces.push(home);
                pages.extend(role.iter().map(|id| BotPage {
                    workspace: home,
                    document: koru_ai_core::DocumentId::from_uuid(*id),
                }));
            }
            Command::Change {
                action: Change::Knowledge { bot, knowledge, .. },
                ..
            } => {
                let home = read_bot(*bot)?.definition.access.workspace;
                workspaces.push(home);
                pages.extend(knowledge.iter().map(|id| BotPage {
                    workspace: home,
                    document: koru_ai_core::DocumentId::from_uuid(*id),
                }));
            }
            Command::Change {
                action:
                    Change::Library {
                        bot,
                        role,
                        knowledge,
                        memory,
                        ..
                    },
                ..
            } => {
                let home = read_bot(*bot)?.definition.access.workspace;
                workspaces.push(home);
                pages.extend(
                    role.iter()
                        .chain(knowledge)
                        .chain(memory.iter())
                        .map(|id| BotPage {
                            workspace: home,
                            document: koru_ai_core::DocumentId::from_uuid(*id),
                        }),
                );
            }
            Command::Change {
                action: Change::Access { bot, access, .. },
                ..
            } => {
                if read_bot(*bot)?.definition.access.workspace != access.workspace {
                    return Err(Error::NotAllowed);
                }
                workspaces.push(access.workspace);
                pages.extend(access.selected_pages.iter().map(|document| BotPage {
                    workspace: access.workspace,
                    document: *document,
                }));
            }
            Command::Read {
                query:
                    Query::Bots { workspace, .. }
                    | Query::Activity { workspace }
                    | Query::TrashedConversations { workspace, .. },
            } => workspaces.push(koru_ai_core::WorkspaceId::from_uuid(*workspace)),
            Command::Read {
                query: Query::Conversations { bot, .. },
            } => {
                workspaces.push(read_bot(*bot)?.definition.access.workspace);
            }
            Command::Send { envelope } => {
                // The attachment owner is composed with the shared composer in
                // P1.7. Until then reject, never accept files that cannot travel.
                if !envelope.attachments.is_empty() {
                    return Err(Error::NotAllowed);
                }
                workspaces.push(read_bot(envelope.to)?.definition.access.workspace);
            }
            _ => {}
        }
        if !workspaces.is_empty() {
            if pages.len() > 1042 {
                return Err(Error::InvalidRequest);
            }
            let snapshot = self
                .authority
                .inspect(&workspaces, &pages)
                .map_err(authority_error)?;
            if snapshot.owner != owner {
                return Err(Error::NotAllowed);
            }
            for workspace in workspaces {
                snapshot
                    .workspaces
                    .get(&workspace)
                    .ok_or(Error::NotAllowed)?
                    .as_ref()
                    .map_err(|error| authority_error(*error))?;
            }
            for page in pages {
                snapshot
                    .pages
                    .get(&page)
                    .ok_or(Error::NotAllowed)?
                    .as_ref()
                    .map_err(|error| authority_error(*error))?;
            }
        }
        send(command)
    }
}
pub(crate) async fn command(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<Command>, JsonRejection>,
) -> Response {
    if !permitted(&principal) {
        return failed(Error::NotAllowed);
    }
    let Ok(Json(command)) = payload else {
        return failed(Error::InvalidRequest);
    };
    let Some(gateway) = state.bots_engine.get().cloned() else {
        return failed(Error::EngineUnavailable);
    };
    let Ok(permit) = gateway.permits.clone().try_acquire_owned() else {
        return failed(Error::EngineUnavailable);
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        gateway.execute(&principal, command)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => failed(error),
        Err(_) => failed(Error::EngineUnavailable),
    }
}
pub(crate) async fn window_command(
    state: Arc<GatewayState>,
    principal: GatewayPrincipal,
    command: koru_ai_core::bots::BotCommand,
) -> Response {
    if !permitted(&principal) {
        return window_failed(Error::NotAllowed);
    }
    let Some(gateway) = state.bots_engine.get().cloned() else {
        return window_failed(Error::EngineUnavailable);
    };
    let Ok(permit) = gateway.permits.clone().try_acquire_owned() else {
        return window_failed(Error::EngineUnavailable);
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        gateway.window_command(command)
    })
    .await
    {
        Ok(Ok(reply)) => Json(reply).into_response(),
        Ok(Err(error)) => window_failed(error),
        Err(_) => window_failed(Error::EngineUnavailable),
    }
}
fn window_failed(error: Error) -> Response {
    let (status, code) = match error {
        Error::NotAllowed | Error::Unauthenticated => {
            (StatusCode::FORBIDDEN, BotError::Forbidden.code())
        }
        Error::InvalidRequest | Error::InputTooSmall => {
            (StatusCode::BAD_REQUEST, BotError::Invalid.code())
        }
        Error::Conflict => (StatusCode::CONFLICT, BotError::Conflict.code()),
        Error::RevisionConflict => (StatusCode::CONFLICT, BotError::RevisionConflict.code()),
        Error::TeamRevisionConflict => {
            (StatusCode::CONFLICT, BotError::TeamRevisionConflict.code())
        }
        Error::FolderFrozen => (StatusCode::CONFLICT, BotError::FolderLocked.code()),
        Error::StorageUnavailable => (StatusCode::INTERNAL_SERVER_ERROR, BotError::Storage.code()),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "bot_unavailable"),
    };
    crate::failure(status, code, "The Bot request could not be completed.")
}
fn failed(error: Error) -> Response {
    let status = match error {
        Error::NotAllowed | Error::Unauthenticated => StatusCode::FORBIDDEN,
        Error::InvalidRequest | Error::InputTooSmall => StatusCode::BAD_REQUEST,
        Error::EngineUnavailable | Error::StorageUnavailable | Error::EngineDisabled => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::CONFLICT,
    };
    crate::failure(
        status,
        &error.to_string(),
        "The Bot request could not be completed.",
    )
}
#[cfg(test)]
mod tests;
