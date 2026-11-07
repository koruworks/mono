// SPDX-License-Identifier: MPL-2.0

//! First-party grants: the Agent saying the process asking is the Koru app.
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
//! The Agent can, from two operating-system answers. When a connection arrives
//! on the Agent's endpoint, the pid on the other end is read off the transport
//! — `GetNamedPipeClientProcessId` / the socket's peer credentials — and is
//! therefore the operating system's answer rather than the peer's claim. When
//! that connection asks for a grant, the operating system is asked which file
//! that pid is running, and the answer must be the `koru` beside the Agent's
//! own executable — the binary it would launch itself, never one found through
//! `PATH`. The two together are the anchor: *this connection belongs to this
//! installation's Koru app*. How the window was started does not enter into
//! it, so a window opened from Finder or the Start menu, one opened while an
//! Agent was already running, and one the Agent spawned are trusted alike; see
//! [`identity`] and ADR-267's 2026-11-06 amendment.
//!
//! ## What this is not
//!
//! It is not proof against a determined local process. On a single-user desktop
//! any same-user mechanism is advisory: a process running as the person can
//! reach the Agent's endpoint, and it can start the real `koru` beside the
//! Agent and drive it. What the grant stops is a *well-behaved* tool — an MCP
//! server, a plugin, a script — stumbling into a destructive operation because
//! it asked politely and nothing said no. That is the value the SP-8 plan
//! claims for it, and it is the value it has.
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

mod identity;
pub(crate) use identity::KoruPeer;

/// How long a minted grant is good for.
///
/// It has to cover one hop — a window asks the Agent, then connects to the
/// Gateway, which asks the Agent back — and nothing else. Two minutes is
/// generous for that and short enough that a token found in a crash dump is
/// almost certainly already dead. **Open**: whether this is the right number is
/// a security-shape question recorded in the ADR rather than settled here.
pub const GRANT_LIFETIME_MILLIS: i64 = 120_000;

/// The ledger, with no clock, no socket and no global.
///
/// Who may have a grant is decided before this is reached — by
/// [`identity::verify`], whose [`KoruPeer`] is the only way in — so what is
/// left here is issuing, spending and expiring, all assertable without a
/// process.
#[derive(Debug, Default)]
pub struct GrantAuthority {
    /// token → when it expires. Removed when spent.
    issued: HashMap<String, i64>,
}

/// Why a grant was refused. Never sent to the caller — the wire answers a bare
/// `not_first_party`, because saying which would tell a guessing caller which
/// guesses are getting warmer. It is written to the Agent's routing log.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The transport gave no pid, or the operating system would not say which
    /// file that pid is running, or a path would not resolve.
    Unverified,
    /// The peer is running some other file than the `koru` beside this Agent:
    /// another program, or a Koru from another installation.
    NotThisKoru,
}

impl GrantAuthority {
    /// Mint a grant for a peer [`identity::verify`] said is the Koru app.
    pub(crate) fn mint(
        &mut self,
        _peer: KoruPeer,
        now_millis: i64,
        token: String,
    ) -> koru_agent_wire::FirstPartyGrant {
        self.expire(now_millis);
        let expires_at_millis = now_millis.saturating_add(GRANT_LIFETIME_MILLIS);
        self.issued.insert(token.clone(), expires_at_millis);
        koru_agent_wire::FirstPartyGrant::new(token, expires_at_millis)
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
/// A global rather than a field on `Agent`, so the free functions below are the
/// whole of how the event loop reaches it. A mutex around one map is not
/// something anything waits on for long — the operating-system reads are made
/// before it is taken.
fn authority() -> &'static Mutex<GrantAuthority> {
    static AUTHORITY: std::sync::OnceLock<Mutex<GrantAuthority>> = std::sync::OnceLock::new();
    AUTHORITY.get_or_init(|| Mutex::new(GrantAuthority::default()))
}

fn with<T>(action: impl FnOnce(&mut GrantAuthority) -> T) -> T {
    action(&mut authority().lock().unwrap_or_else(|held| held.into_inner()))
}

/// Mint a grant for the connection whose pid the transport reported, or refuse.
pub fn mint(peer_pid: Option<u32>) -> Result<koru_agent_wire::FirstPartyGrant, Refusal> {
    let peer = identity::verify(peer_pid, &identity::Os)?;
    Ok(with(|authority| {
        authority.mint(peer, now_millis(), uuid::Uuid::new_v4().to_string())
    }))
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
