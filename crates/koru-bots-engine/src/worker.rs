// SPDX-License-Identifier: MPL-2.0

//! Small worker control surface. The engine actor alone owns SQLite; bounded
//! connection threads authenticate and pass commands to it without disk I/O.

mod server;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Status};

const WIRE_VERSION: u8 = 1;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub launch_id: Uuid,
    pub segment: String,
    pub root: PathBuf,
    pub authority: String,
}

impl std::fmt::Debug for Bootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bootstrap").finish_non_exhaustive()
    }
}

pub fn endpoint(root: &Path, segment: &str) -> Result<koru_ipc::Endpoint, Error> {
    koru_ipc::Endpoint::for_service_channel_root(koru_ipc::BOTS, segment, root)
        .map_err(|_| Error::EngineUnavailable)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Hello,
    Status,
    Capacity,
    RefreshCapacity,
    ClockChanged,
    Person(crate::person::Request),
    Attachment(crate::attachments::Request),
    Creation(crate::creation::Request),
    Operation(crate::operations::control::Command),
    SessionCatalog {
        secret: crate::authority::Secret,
    },
    ResolveInput {
        secret: crate::authority::Secret,
        turn: crate::messages::Turn,
        delivery: Uuid,
    },
    ResolveCall {
        secret: crate::authority::Secret,
        binding: Uuid,
        descriptor: crate::authority::Descriptor,
    },
    Stop,
    WakeBot {
        bot: Uuid,
    },
    StopBot {
        bot: Uuid,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    authority: String,
    command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Hello { launch_id: Uuid },
    Status(Status),
    Stopping,
    Queued,
    Capacity { sessions: u32 },
    Resolved(crate::authority::Resolution),
    Input(crate::authority::input::Grant),
    Catalog(CatalogGrant),
    Person(crate::person::Reply),
    Attachment(crate::attachments::Reply),
    Creation(crate::creation::Reply),
    Operation(crate::operations::control::Reply),
    Refused(Error),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CatalogGrant(pub serde_json::Value);
impl std::fmt::Debug for CatalogGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CatalogGrant(<redacted>)")
    }
}

pub fn request(
    endpoint: &koru_ipc::Endpoint,
    authority: &str,
    command: Command,
) -> Result<Reply, Error> {
    let mut stream =
        koru_ipc::connect_timed(endpoint, TIMEOUT).map_err(|_| Error::EngineUnavailable)?;
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    koru_ipc::frame::write_frame(
        &mut stream,
        WIRE_VERSION,
        &Request {
            authority: authority.into(),
            command,
        },
    )
    .map_err(|_| Error::EngineUnavailable)?;
    let (version, reply) =
        koru_ipc::frame::read_frame(&mut stream).map_err(|_| Error::EngineUnavailable)?;
    if version != WIRE_VERSION {
        return Err(Error::InvalidRequest);
    }
    Ok(reply)
}

pub fn run_supervised(root: &Path) -> Result<(), Error> {
    run_supervised_with_policy(root, crate::launcher::Policy::default())
}

/// Allows the explicit process harness to use a short idle/registration clock.
/// Production dispatch always uses the default policy above.
pub fn run_supervised_with_policy(
    root: &Path,
    policy: crate::launcher::Policy,
) -> Result<(), Error> {
    let mut input = std::io::stdin();
    let bytes =
        koru_agent_worker::read_bootstrap_line(&mut input).map_err(|_| Error::InvalidRequest)?;
    let bootstrap: Bootstrap = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidRequest)?;
    if bootstrap.root != root
        || bootstrap.segment != koru_channel::segment()
        || bootstrap.launch_id.is_nil()
        || bootstrap.authority.len() != 64
        || !bootstrap.authority.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::InvalidRequest);
    }
    server::serve(bootstrap, input, policy)
}
