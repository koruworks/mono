// SPDX-License-Identifier: MPL-2.0

//! First-party grants: the Agent saying it launched whoever is asking.
//!
//! R9 asks for a destructive permission granted "only to trusted first-party
//! windows", and SP-6a's survey found the mechanism did not exist: every local
//! caller is admitted to the Gateway by channel and API version, `product` is a
//! free-form string every `koru-api-client` caller sets identically, and scopes
//! are request-filtered rather than caller-proven. Naming a scope raised the
//! bar from "any local caller, silently" to "a caller that deliberately asked".
//! It could not raise it further, because nothing in the tree could answer
//! *who is asking*.
//!
//! The Agent can, for one population and no other: **the processes it started
//! itself**. It resolves the binary beside its own executable, never through
//! `PATH`, and the operating system tells it the child's pid. When a connection
//! arrives on the Agent's endpoint, the pid on the other end is read off the
//! transport — `GetNamedPipeClientProcessId` / `SO_PEERCRED` — and is therefore
//! the operating system's answer rather than the peer's claim. The two together
//! are the anchor: *this connection belongs to a process I launched*.
//!
//! **One process it did not launch is trusted too: the one that launched it.**
//! A cold start from Finder, the Start menu or a shortcut runs `koru` first,
//! and `koru` starts the Agent — so the first window was never in the table.
//! At startup the Agent asks the operating system for its parent's pid and
//! that parent's executable, and records the pid when the executable is the
//! `koru` beside its own; see [`launcher`] and ADR-267's 2026-11-06
//! amendment.
//!
//! ## What this is not
//!
//! It is not proof against a determined local process. On a single-user desktop
//! any same-user mechanism is advisory: a process running as the person can
//! read the Agent's endpoint, and a pid this Agent launched can in principle be
//! recycled by the operating system after that process exits and claimed by
//! another. What the grant stops is a *well-behaved* tool — an MCP server, a
//! plugin, a script — stumbling into a destructive operation because it asked
//! politely and nothing said no. That is the value the SP-8 plan claims for it,
//! and it is the value it has.
//!
//! It is also not a permission. A grant carries a token and an expiry; the
//! scope system stays the authorization vocabulary, because two vocabularies
//! would be two places to look when something is allowed that should not be.
//!
//! ## Spent on validation
//!
//! A grant is consumed by the Gateway validating it. A window asks for one when
//! it opens its Gateway session and presents it once; a token that leaks after
//! that is already worthless. This also bounds the table: nothing accumulates
//! but grants nobody redeemed, and those expire.

use std::collections::HashMap;
use std::sync::Mutex;

mod launcher;
pub use launcher::trust_launcher;

/// How long a minted grant is good for.
///
/// It has to cover one hop — a window asks the Agent, then connects to the
/// Gateway, which asks the Agent back — and nothing else. Two minutes is
/// generous for that and short enough that a token found in a crash dump is
/// almost certainly already dead. **Open**: whether this is the right number is
/// a security-shape question recorded in the ADR rather than settled here.
pub const GRANT_LIFETIME_MILLIS: i64 = 120_000;

/// A launched pid is remembered for the Agent's life, but only up to this many
/// of them, oldest evicted first. A person who opens and closes windows all day
/// must not grow this table without bound, and a window that has been gone long
/// enough to fall off the end has no grant to ask for.
const LAUNCHED_CAPACITY: usize = 256;

/// The decision, with no clock, no socket and no global.
///
/// Everything this module refuses is refused here, which is why the whole of it
/// is assertable without launching a process.
#[derive(Debug, Default)]
pub struct GrantAuthority {
    /// pid → when this Agent launched it. Insertion-ordered by the millis.
    launched: HashMap<u32, i64>,
    /// token → when it expires. Removed when spent.
    issued: HashMap<String, i64>,
}

/// Why a grant was refused. Never sent to the caller — the wire answers a bare
/// `false`, because saying which of the three would tell a guessing caller
/// which guesses are getting warmer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The transport gave no pid, or one this Agent did not start.
    NotLaunchedHere,
}

impl GrantAuthority {
    /// Record a process this Agent started, or the `koru` that started it.
    /// Called from every spawn path and once at startup, so the table is a
    /// fact about launches rather than about intentions.
    pub fn launched(&mut self, pid: u32, at_millis: i64) {
        if self.launched.len() >= LAUNCHED_CAPACITY
            && let Some(oldest) = self
                .launched
                .iter()
                .min_by_key(|(pid, at)| (**at, **pid))
                .map(|(pid, _)| *pid)
        {
            self.launched.remove(&oldest);
        }
        self.launched.insert(pid, at_millis);
    }

    pub fn was_launched_here(&self, pid: u32) -> bool {
        self.launched.contains_key(&pid)
    }

    /// Mint a grant for a connection whose transport-read pid this Agent
    /// launched.
    ///
    /// `peer_pid` is `None` when the platform could not answer, and `None` is a
    /// refusal: a mechanism that fell back to trusting the caller when it could
    /// not check would be a mechanism that never checked.
    pub fn mint(
        &mut self,
        peer_pid: Option<u32>,
        now_millis: i64,
        token: String,
    ) -> Result<koru_agent_wire::FirstPartyGrant, Refusal> {
        let Some(pid) = peer_pid.filter(|pid| self.was_launched_here(*pid)) else {
            return Err(Refusal::NotLaunchedHere);
        };
        let _ = pid;
        self.expire(now_millis);
        let expires_at_millis = now_millis.saturating_add(GRANT_LIFETIME_MILLIS);
        self.issued.insert(token.clone(), expires_at_millis);
        Ok(koru_agent_wire::FirstPartyGrant::new(
            token,
            expires_at_millis,
        ))
    }

    /// Spend a grant. `true` exactly once, for a token this Agent minted and
    /// that has not expired.
    pub fn validate(&mut self, token: &str, now_millis: i64) -> bool {
        self.expire(now_millis);
        self.issued.remove(token).is_some()
    }

    fn expire(&mut self, now_millis: i64) {
        self.issued.retain(|_, expires| *expires > now_millis);
    }
}

/// The one authority this process holds.
///
/// A global rather than a field on `Agent`, because the two halves it joins do
/// not meet anywhere else: launches happen in free spawn functions and on
/// worker threads, and the requests arrive on the event loop. A mutex around a
/// two-map struct is not something either of those can wait on for long.
fn authority() -> &'static Mutex<GrantAuthority> {
    static AUTHORITY: std::sync::OnceLock<Mutex<GrantAuthority>> = std::sync::OnceLock::new();
    AUTHORITY.get_or_init(|| Mutex::new(GrantAuthority::default()))
}

fn with<T>(action: impl FnOnce(&mut GrantAuthority) -> T) -> T {
    action(&mut authority().lock().unwrap_or_else(|held| held.into_inner()))
}

/// Remember a process this Agent just started. (The one it did not start, its
/// launcher, goes into the same table through [`trust_launcher`].) Every spawn
/// path calls it, which
/// is why it takes a bare pid: what matters is that the operating system gave
/// this number back from *our* `spawn`, not which product it was.
pub fn record_launch(pid: u32) {
    with(|authority| authority.launched(pid, now_millis()));
}

/// Mint a grant for the connection whose pid the transport reported, or refuse.
pub fn mint(peer_pid: Option<u32>) -> Result<koru_agent_wire::FirstPartyGrant, Refusal> {
    with(|authority| authority.mint(peer_pid, now_millis(), uuid::Uuid::new_v4().to_string()))
}

/// Spend a presented grant.
pub fn validate(token: &str) -> bool {
    with(|authority| authority.validate(token, now_millis()))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests;
