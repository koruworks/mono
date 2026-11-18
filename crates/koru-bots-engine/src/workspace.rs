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
    Refused(Error),
}

pub fn endpoint(root: &Path, segment: &str) -> Result<koru_ipc::Endpoint, Error> {
    koru_ipc::Endpoint::for_service_channel_root(koru_ipc::BOT_WORKSPACE, segment, root)
        .map_err(|_| Error::EngineUnavailable)
}
pub fn request(endpoint: &koru_ipc::Endpoint, request: &Request) -> Result<Value, Error> {
    request.call()?;
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
    match reply {
        Reply::Result(value) => Ok(value),
        Reply::Refused(error) => Err(error),
    }
}
