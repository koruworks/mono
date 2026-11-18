// SPDX-License-Identifier: MPL-2.0

//! The worker dials the engine. Its bootstrap grants one registration of one
//! incarnation; the resulting connection, not a reusable bearer, is authority.

use crate::{Error, bot_database::BotDatabase};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub(crate) const VERSION: u8 = 1;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub bot: Uuid,
    pub incarnation: Uuid,
    pub root: PathBuf,
    pub segment: String,
    pub capability: String,
}
impl std::fmt::Debug for Bootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotBootstrap").finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Registration {
    pub bot: Uuid,
    pub incarnation: Uuid,
    pub capability: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Reply {
    Registered,
    Control(crate::carrier::Reply),
    Stop,
    Refused,
}

pub fn endpoint(root: &Path, segment: &str) -> Result<koru_ipc::Endpoint, Error> {
    koru_ipc::Endpoint::for_service_channel_root(koru_ipc::BOT_WORKERS, segment, root)
        .map_err(|_| Error::EngineUnavailable)
}

pub fn run_supervised(bot: Uuid) -> Result<(), Error> {
    let mut input = std::io::stdin();
    let bytes =
        koru_agent_worker::read_bootstrap_line(&mut input).map_err(|_| Error::InvalidRequest)?;
    let bootstrap: Bootstrap = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidRequest)?;
    if bot.is_nil()
        || bootstrap.bot != bot
        || bootstrap.incarnation.is_nil()
        || !bootstrap.root.is_absolute()
        || bootstrap.segment != koru_channel::segment()
        || bootstrap.capability.len() != 64
        || !bootstrap.capability.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::InvalidRequest);
    }
    let parent_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let alive = parent_alive.clone();
    koru_agent_worker::ParentPipe::new(input)
        .watch("koru-bot parent", move || {
            alive.store(false, std::sync::atomic::Ordering::Release);
        })
        .map_err(|_| Error::EngineUnavailable)?;
    let mut stream =
        koru_ipc::connect_timed(&endpoint(&bootstrap.root, &bootstrap.segment)?, TIMEOUT)
            .map_err(|_| Error::EngineUnavailable)?;
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    koru_ipc::frame::write_frame(
        &mut stream,
        VERSION,
        &Registration {
            bot,
            incarnation: bootstrap.incarnation,
            capability: bootstrap.capability,
        },
    )
    .map_err(|_| Error::EngineUnavailable)?;
    let (version, reply) = koru_ipc::frame::read_frame::<_, Reply>(&mut stream)
        .map_err(|_| Error::EngineUnavailable)?;
    if version != VERSION || !matches!(reply, Reply::Registered) {
        return Err(Error::Unauthenticated);
    }
    let _database = BotDatabase::open(&bootstrap.root, bot)?;
    while parent_alive.load(std::sync::atomic::Ordering::Acquire) {
        // P1.4 replaces this empty shell with its owned provider-session set.
        koru_ipc::frame::write_frame(&mut stream, VERSION, &crate::carrier::Command::Poll)
            .map_err(|_| Error::EngineUnavailable)?;
        let (version, reply) = koru_ipc::frame::read_frame::<_, Reply>(&mut stream)
            .map_err(|_| Error::EngineUnavailable)?;
        if version != VERSION {
            return Err(Error::InvalidRequest);
        }
        match reply {
            Reply::Control(crate::carrier::Reply::Work { starts, stops })
                if starts.is_empty() && stops.is_empty() => {}
            Reply::Stop => return Ok(()),
            _ => return Err(Error::Unauthenticated),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}
