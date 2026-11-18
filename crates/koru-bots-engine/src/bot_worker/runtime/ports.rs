// SPDX-License-Identifier: MPL-2.0
use super::client::Client;
use crate::{
    Error,
    authority::{Binding, Call, Mint, Secret},
    bot_database::{BotDatabase, JournalEntry},
    capacity::Reservation,
    carrier::{Command, Reply},
    door,
    messages::{Delivery, Resume, Turn},
    sessions, workspace,
};
use koru_session_protocol::{AgentEvent, DelegationLaunch, LaunchSelection};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub(super) struct Port {
    pub client: Client,
    pub reservation: Reservation,
    pub selection: LaunchSelection,
    pub workspace: koru_ipc::Endpoint,
    pub endpoint: koru_session_protocol::McpEndpoint,
    pub registry: Arc<door::http::Registry>,
    pub database: Arc<Mutex<BotDatabase>>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    secret: Option<Secret>,
    turn: Option<Turn>,
}
impl Port {
    pub fn new(
        client: Client,
        reservation: Reservation,
        selection: LaunchSelection,
        workspace: koru_ipc::Endpoint,
        endpoint: koru_session_protocol::McpEndpoint,
        registry: Arc<door::http::Registry>,
        database: Arc<Mutex<BotDatabase>>,
    ) -> Self {
        Self {
            client,
            reservation,
            selection,
            workspace,
            endpoint,
            registry,
            database,
            state: Mutex::new(State::default()),
        }
    }
    pub fn turn(&self) -> Option<Turn> {
        self.state.lock().ok().and_then(|s| s.turn)
    }
    fn thread(&self, thread: Uuid) -> Result<(), Error> {
        if self.reservation.key.thread == thread {
            Ok(())
        } else {
            Err(Error::NotAllowed)
        }
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            if let Some(secret) = &state.secret {
                self.registry.remove(secret);
            }
        }
    }
}
impl sessions::Port for Port {
    fn context(&self, thread: Uuid) -> Result<sessions::Context, Error> {
        self.thread(thread)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.client.call(Command::Context {
                reservation: self.reservation,
            })? {
                Reply::Context(context) => return Ok(context),
                Reply::ContextPending if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Reply::ContextPending => return Err(Error::EngineUnavailable),
                _ => return Err(Error::InvalidRequest),
            }
        }
    }
    fn credential(&self, thread: Uuid, command: Uuid) -> Result<sessions::SessionGrant, Error> {
        self.thread(thread)?;
        let mut command = command;
        for _ in 0..3 {
            let predecessor = match self.client.call(Command::Head {
                reservation: self.reservation,
            }) {
                Ok(Reply::Head(head)) => head,
                Err(Error::EngineUnavailable) => {
                    command = Uuid::new_v4();
                    continue;
                }
                Err(error) => return Err(error),
                _ => return Err(Error::InvalidRequest),
            };
            let issued = match self.client.call(Command::Mint {
                reservation: self.reservation,
                request: Mint {
                    command,
                    thread,
                    predecessor,
                },
            }) {
                Ok(Reply::Issued(issued)) => issued,
                Err(Error::EngineUnavailable | Error::SessionConflict) => {
                    command = Uuid::new_v4();
                    continue;
                }
                Err(error) => return Err(error),
                _ => return Err(Error::InvalidRequest),
            };
            let Some(secret) = issued.secret else {
                command = Uuid::new_v4();
                continue;
            };
            let port = Arc::new(ToolPort {
                client: self.client.clone(),
                reservation: self.reservation,
                session: issued.session,
                workspace: self.workspace.clone(),
                database: Arc::clone(&self.database),
            });
            let door = door::Door::open(secret.clone(), port)?;
            let tools = door
                .catalog()?
                .into_iter()
                .map(|t| {
                    t["name"]
                        .as_str()
                        .ok_or(Error::InvalidRequest)?
                        .parse()
                        .map_err(|_| Error::InvalidRequest)
                })
                .collect::<Result<Vec<koru_session_protocol::McpToolName>, Error>>()?;
            self.registry.install(door)?;
            let mut state = self.state.lock().map_err(|_| Error::StorageUnavailable)?;
            if let Some(previous) = state.secret.replace(secret.clone()) {
                self.registry.remove(&previous);
            }
            return Ok(sessions::SessionGrant {
                session: issued
                    .session
                    .to_string()
                    .parse()
                    .map_err(|_| Error::InvalidRequest)?,
                launch: DelegationLaunch {
                    model: self.selection.model.clone(),
                    mcp_endpoint: self.endpoint.clone(),
                    credential: Some(secret.expose().parse().map_err(|_| Error::InvalidRequest)?),
                    tools: tools.try_into().map_err(|_| Error::InvalidRequest)?,
                    resume: None,
                },
            });
        }
        Err(Error::EngineUnavailable)
    }
    fn claim(
        &self,
        thread: Uuid,
        session: Uuid,
        resume: Resume,
        budget: usize,
    ) -> Result<Option<Delivery>, Error> {
        self.thread(thread)?;
        let Reply::Delivery(delivery) = self.client.call(Command::Claim {
            reservation: self.reservation,
            session,
            resume,
            budget,
        })?
        else {
            return Err(Error::InvalidRequest);
        };
        if let Some(delivery) = &delivery {
            self.state
                .lock()
                .map_err(|_| Error::StorageUnavailable)?
                .turn = Some(delivery.turn);
        }
        Ok(delivery)
    }
    fn accepted(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.client.done(Command::Accepted {
            reservation: self.reservation,
            turn,
            delivery,
        })
    }
    fn offer_steer(&self, turn: Turn, budget: usize) -> Result<Option<Delivery>, Error> {
        match self.client.call(Command::OfferSteer {
            reservation: self.reservation,
            turn,
            budget,
        })? {
            Reply::Delivery(delivery) => Ok(delivery),
            _ => Err(Error::InvalidRequest),
        }
    }
    fn reject_steer(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.client.done(Command::RejectSteer {
            reservation: self.reservation,
            turn,
            delivery,
        })
    }
    fn unknown(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.client.done(Command::Unknown {
            reservation: self.reservation,
            turn,
            delivery,
        })
    }
    fn event(&self, turn: Turn, event: &AgentEvent) -> Result<(), Error> {
        self.client.done(Command::Event {
            reservation: self.reservation,
            turn,
            event: event.clone(),
        })
    }
    fn publish(&self, turn: Turn, command: Uuid, body: String) -> Result<(), Error> {
        let request = crate::messages::Publish {
            command,
            turn,
            body,
            outputs: vec![],
        };
        for _ in 0..3 {
            match self.client.call(Command::Publish {
                reservation: self.reservation,
                request: request.clone(),
            }) {
                Ok(Reply::Published(_)) => return Ok(()),
                Err(Error::EngineUnavailable) => continue,
                Err(error) => return Err(error),
                _ => return Err(Error::InvalidRequest),
            }
        }
        Err(Error::EngineUnavailable)
    }
    fn retired(&self, turn: Turn) -> Result<(), Error> {
        // The worker manager releases the assignment only after Pool::finished
        // confirms this session thread itself has returned. It also handles
        // failed starts and lost Claim replies whose turn was never received.
        self.state
            .lock()
            .map_err(|_| Error::StorageUnavailable)?
            .turn = Some(turn);
        Ok(())
    }
}

struct ToolPort {
    client: Client,
    reservation: Reservation,
    session: Uuid,
    workspace: koru_ipc::Endpoint,
    database: Arc<Mutex<BotDatabase>>,
}
impl door::Port for ToolPort {
    fn catalog(&self, secret: &Secret) -> Result<Vec<Value>, Error> {
        let Reply::CatalogGrant(_) = self.client.call(Command::Catalog {
            reservation: self.reservation,
            session: self.session,
            secret: secret.clone(),
        })?
        else {
            return Err(Error::InvalidRequest);
        };
        let mut tools = workspace::catalog(&self.workspace, secret.clone())?;
        tools.extend(crate::own_tools::catalog());
        Ok(tools)
    }
    fn bind(&self, secret: &Secret, command: Uuid, call: &Call) -> Result<Binding, Error> {
        match self.client.call(Command::Bind {
            reservation: self.reservation,
            session: self.session,
            secret: secret.clone(),
            command,
            descriptor: call.descriptor().clone(),
        })? {
            Reply::Bound(binding) => Ok(binding),
            _ => Err(Error::InvalidRequest),
        }
    }
    fn journal(&self, binding: Binding, tool: &str) -> Result<(), Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        self.database
            .lock()
            .map_err(|_| Error::StorageUnavailable)?
            .journal(
                binding.turn.id,
                JournalEntry::ToolCall {
                    name: tool,
                    payload: b"{}",
                },
                now,
            )
    }
    fn execute(&self, secret: &Secret, binding: Binding, call: &Call) -> Result<Value, Error> {
        if crate::own_tools::catalog()
            .iter()
            .any(|t| t["name"].as_str() == Some(call.tool()))
        {
            match self.client.call(Command::OwnTool {
                reservation: self.reservation,
                session: self.session,
                secret: secret.clone(),
                binding: binding.id,
                door_call: call.door_call(),
                tool: call.tool().into(),
                arguments: serde_json::to_vec(call.arguments())
                    .map_err(|_| Error::InvalidRequest)?,
            })? {
                Reply::Tool(value) => Ok(value),
                _ => Err(Error::InvalidRequest),
            }
        } else {
            workspace::request(
                &self.workspace,
                &workspace::Request::new(secret.clone(), binding.id, call)?,
            )
        }
    }
}
