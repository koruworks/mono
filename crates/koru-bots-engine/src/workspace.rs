// SPDX-License-Identifier: MPL-2.0

//! Private worker-to-gateway transport. Session credentials grant no general
//! gateway authority; every request still needs its original engine binding.
use crate::{
    Error,
    authority::{Call, Secret},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};
use uuid::Uuid;

pub mod context;
pub mod input;
pub use context::{Context, Page, Sources};

#[derive(Serialize, Deserialize)]
pub enum Command {
    Call(Request),
    Catalog {
        secret: Secret,
    },
    Input {
        secret: Secret,
        turn: crate::messages::Turn,
        delivery: Uuid,
    },
    Recover {
        authority: Secret,
        request: RecoveryRequest,
    },
    /// This is an engine-to-gateway command. A session bearer cannot invoke it.
    Context {
        authority: Secret,
        sources: Sources,
    },
}

pub const VERSION: u8 = 1;
pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub secret: Secret,
    pub binding: Uuid,
    pub door_call: Uuid,
    pub tool: String,
    pub arguments: Vec<u8>,
}
impl Request {
    pub fn new(secret: Secret, binding: Uuid, call: &Call) -> Result<Self, Error> {
        Ok(Self {
            secret,
            binding,
            door_call: call.door_call(),
            tool: call.tool().into(),
            arguments: serde_json::to_vec(call.arguments()).map_err(|_| Error::InvalidRequest)?,
        })
    }
    /// Recompute from exactly the arguments the gateway will execute. A caller
    /// cannot supply a different descriptor alongside a valid binding.
    pub fn call(&self) -> Result<Call, Error> {
        if self.binding.is_nil() {
            return Err(Error::InvalidRequest);
        }
        Call::parse(self.door_call, self.tool.clone(), &self.arguments)
    }
}
#[derive(Serialize, Deserialize)]
pub enum Reply {
    Result(Value),
    Context(Context),
    Input(input::Prepared),
    Recovery(RecoveryReply),
    Refused(Error),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum RecoveryRequest {
    Fence {
        record: crate::operations::Record,
        force: bool,
    },
    Lookup {
        record: crate::operations::Record,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub enum RecoveryReply {
    Pending,
    Drained(koru_store_core::NativeEffectDrainReceipt),
    Evidence(Option<crate::operations::Outcome>),
}
pub fn recover(
    endpoint: &koru_ipc::Endpoint,
    authority: Secret,
    request: RecoveryRequest,
) -> Result<RecoveryReply, Error> {
    match send(endpoint, &Command::Recover { authority, request })? {
        Reply::Recovery(reply) => Ok(reply),
        Reply::Refused(error) => Err(error),
        _ => Err(Error::InvalidRequest),
    }
}

pub fn endpoint(root: &Path, segment: &str) -> Result<koru_ipc::Endpoint, Error> {
    koru_ipc::Endpoint::for_service_channel_root(koru_ipc::BOT_WORKSPACE, segment, root)
        .map_err(|_| Error::EngineUnavailable)
}
pub fn request(endpoint: &koru_ipc::Endpoint, request: &Request) -> Result<Value, Error> {
    request.call()?;
    let wire = Command::Call(Request {
        secret: request.secret.clone(),
        binding: request.binding,
        door_call: request.door_call,
        tool: request.tool.clone(),
        arguments: request.arguments.clone(),
    });
    match send(endpoint, &wire)? {
        Reply::Result(value) => Ok(value),
        Reply::Refused(error) => Err(error),
        _ => Err(Error::InvalidRequest),
    }
}

pub fn load_context(
    endpoint: &koru_ipc::Endpoint,
    authority: Secret,
    sources: Sources,
) -> Result<Context, Error> {
    sources.validate()?;
    match send(endpoint, &Command::Context { authority, sources })? {
        Reply::Context(context) => Ok(context),
        Reply::Refused(error) => Err(error),
        _ => Err(Error::InvalidRequest),
    }
}

pub fn catalog(endpoint: &koru_ipc::Endpoint, secret: Secret) -> Result<Vec<Value>, Error> {
    match send(endpoint, &Command::Catalog { secret })? {
        Reply::Result(value) => serde_json::from_value(value).map_err(|_| Error::InvalidRequest),
        Reply::Refused(error) => Err(error),
        _ => Err(Error::InvalidRequest),
    }
}

pub fn prepare_input(
    endpoint: &koru_ipc::Endpoint,
    secret: Secret,
    turn: crate::messages::Turn,
    delivery: Uuid,
) -> Result<input::Prepared, Error> {
    match send(
        endpoint,
        &Command::Input {
            secret,
            turn,
            delivery,
        },
    )? {
        Reply::Input(input) => Ok(input),
        Reply::Refused(error) => Err(error),
        _ => Err(Error::InvalidRequest),
    }
}

fn send(endpoint: &koru_ipc::Endpoint, request: &Command) -> Result<Reply, Error> {
    let mut stream =
        koru_ipc::connect_timed(endpoint, TIMEOUT).map_err(|_| Error::EngineUnavailable)?;
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    koru_ipc::frame::write_frame(&mut stream, VERSION, request)
        .map_err(|_| Error::EngineUnavailable)?;
    let (version, reply) =
        koru_ipc::frame::read_frame(&mut stream).map_err(|_| Error::EngineUnavailable)?;
    if version != VERSION {
        return Err(Error::InvalidRequest);
    }
    Ok(reply)
}
