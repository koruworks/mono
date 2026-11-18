// SPDX-License-Identifier: MPL-2.0

//! The worker's tool arrival boundary. HTTP owns framing and deadlines; this
//! object binds the exact parsed call before any workspace operation can run.
//! All methods run on a bounded connection task, never heartbeat dispatch.

use crate::{
    Error,
    authority::{Binding, Call, Secret, calls::arguments::Unique},
};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[cfg(feature = "provider-runtime")]
pub mod http;

pub trait Port: Send + Sync + 'static {
    /// Authenticate on discovery, including before a turn opens. Definitions
    /// come from the session's captured thread grant and contain no resources.
    fn catalog(&self, secret: &Secret) -> Result<Vec<Value>, Error>;
    fn bind(&self, secret: &Secret, command: Uuid, call: &Call) -> Result<Binding, Error>;
    /// Record the actual MCP tool name in the Bot's private turn journal.
    fn journal(&self, binding: Binding, tool: &str) -> Result<(), Error>;
    /// The workspace boundary re-resolves the original binding and descriptor
    /// on every execution. An uncertain retry retains this Call and Binding.
    fn execute(&self, secret: &Secret, binding: Binding, call: &Call) -> Result<Value, Error>;
}

pub struct Door {
    secret: Secret,
    port: Arc<dyn Port>,
    allowed: BTreeSet<String>,
}

impl Door {
    pub fn open(secret: Secret, port: Arc<dyn Port>) -> Result<Self, Error> {
        let tools = port.catalog(&secret)?;
        let mut allowed = BTreeSet::new();
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidRequest)?;
            if name.is_empty() || name.len() > 256 || !allowed.insert(name.to_owned()) {
                return Err(Error::InvalidRequest);
            }
        }
        Ok(Self {
            secret,
            port,
            allowed,
        })
    }

    pub fn catalog(&self) -> Result<Vec<Value>, Error> {
        // Reauthenticate rather than letting a revoked credential discover
        // the cached catalog. The snapshot's allowlist never grows in place.
        Ok(self
            .port
            .catalog(&self.secret)?
            .into_iter()
            .filter(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| self.allowed.contains(name))
            })
            .collect())
    }

    /// MCP tools/call params, still raw so duplicate keys cannot disappear in
    /// an outer parser. JSON-RPC IDs and unqualified _meta IDs confer no identity.
    pub fn call(&self, raw: &[u8]) -> Result<Value, Error> {
        if raw.len() > koru_bot_client_wire::MAX_FRAME_BYTES {
            return Err(Error::InvalidRequest);
        }
        let Unique(mut params) = serde_json::from_slice(raw).map_err(|_| Error::InvalidRequest)?;
        let params = params.as_object_mut().ok_or(Error::InvalidRequest)?;
        let name = params
            .remove("name")
            .and_then(|name| name.as_str().map(str::to_owned))
            .ok_or(Error::InvalidRequest)?;
        if !self.allowed.contains(&name) {
            return Err(Error::NotAllowed);
        }
        let arguments = params
            .remove("arguments")
            .unwrap_or_else(|| serde_json::json!({}));
        let call = Call::from_value(Uuid::new_v4(), name, arguments)?;
        let binding = self.port.bind(&self.secret, Uuid::new_v4(), &call)?;
        self.port.journal(binding, call.tool())?;
        self.port.execute(&self.secret, binding, &call)
    }
}

#[cfg(test)]
mod tests;
