// SPDX-License-Identifier: MPL-2.0

//! **The one line the supervising Store writes on stdin, read before anything
//! else.**
//!
//! `koru-agent-worker` spawns this program, puts it in its memory-limited Job
//! Object, and *then* writes [`WorkerBootstrap`] as one line of JSON. Anything
//! this process allocated before the line arrived was allocated outside the
//! ceiling, so [`read`] is the first thing a supervised launch does — before
//! logging, before the endpoint, before any reading machinery exists — and it
//! reads through `koru_agent_worker::read_bootstrap_line`, which is bounded at
//! 64 KiB and stops at the newline. The pipe stays open behind it: its end is
//! the parent's death, which `ParentPipe` watches.
//!
//! The shape lives in the contract (`koru_read::WorkerBootstrap`) so the
//! Store writes it without linking this crate.

use koru_read::{BOOTSTRAP_VERSION, WorkerBootstrap};

/// The longest launch id accepted. It is echoed in every status answer and
/// may be logged, so it is held to a short printable token.
const MAX_LAUNCH_ID: usize = 128;

/// Read and parse the bootstrap line.
pub fn read(reader: &mut impl std::io::Read) -> Result<WorkerBootstrap, String> {
    let line = koru_agent_worker::read_bootstrap_line(reader)
        .map_err(|error| format!("the Store's bootstrap could not be read: {error}"))?;
    parse(&line)
}

/// Parse one line's bytes. Separate from [`read`] so it is tested without a
/// pipe.
pub fn parse(line: &[u8]) -> Result<WorkerBootstrap, String> {
    serde_json::from_slice(line)
        .map_err(|error| format!("the Store's bootstrap is not readable: {error}"))
}

/// Check that this bootstrap is for this worker, on this channel.
///
/// The root is not compared: it is on the command line, and the endpoint is
/// derived from it, so a Store that launched this worker on another root would
/// dial a name this worker never binds and find nothing — a failed readiness
/// probe, not a worker serving the wrong Store.
pub fn validate(bootstrap: &WorkerBootstrap, channel: &str) -> Result<(), String> {
    if bootstrap.bootstrap_version != BOOTSTRAP_VERSION {
        return Err(format!(
            "the Store's bootstrap is version {} and this worker reads {BOOTSTRAP_VERSION}",
            bootstrap.bootstrap_version
        ));
    }
    if bootstrap.channel != channel {
        return Err(format!(
            "the Store's bootstrap is for the {} channel and this worker is {channel}",
            bootstrap.channel
        ));
    }
    let id = &bootstrap.launch_id;
    if id.is_empty() || id.len() > MAX_LAUNCH_ID || !id.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("the Store's bootstrap carries a launch id that is not a token".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
