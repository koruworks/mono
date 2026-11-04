// SPDX-License-Identifier: MPL-2.0

//! Which shell is showing what, and which one an `Open` should go to.
//!
//! ## One entry per connection
//!
//! Not per process, and not per window. One `koru.exe` may hold several windows
//! and route between them internally — `View::open_search_target` already does
//! — and one machine may run several. **A connection is the only thing that is
//! exactly one shell's claim**, which is also why nothing here is keyed on a
//! pid: the pid is a capability for foregrounding, and on a platform that will
//! not report one it is simply absent.
//!
//! ## The agent owns the ordering
//!
//! A shell sends a bare `Active` and no time at all. The order is stamped here,
//! from one monotonic counter, on receipt.
//!
//! A shell-supplied timestamp would be several machines' clocks: they drift, a
//! laptop resumes with the wrong one, and a shell paused in a debugger reports a
//! time from before it was paused. The only order that can be trusted is the
//! order one process observed the messages in — which is this one.
//!
//! ## Disconnect is EOF, and nothing else
//!
//! No heartbeat and no liveness poll. A reader thread ending — clean close,
//! crash, or kill — removes the entry, which is the same argument the
//! rendezvous makes about liveness: you cannot read from a socket nobody is
//! holding. A stale entry would send an `Open` into a closed socket and lose it
//! silently, so a failed write removes the entry too and the routing decision is
//! made again.
//!
//! In memory only. Nothing that describes what is *running* belongs on a disk.

use std::collections::HashMap;

use koru_store_core::ids::{ProfileId, WorkspaceId};

/// Which connection an entry belongs to. Minted by the listener, one per
/// accepted socket, and never reused within a run.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ConnectionId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shell {
    pub connection: ConnectionId,
    /// What the operating system said, never what the message claimed. `None`
    /// where the platform will not say — the connection is still the identity,
    /// and only foregrounding degrades.
    pub pid: Option<u32>,
    pub profile: ProfileId,
    pub workspace: WorkspaceId,
    pub profile_name: String,
    pub workspace_name: String,
    /// When this connection was accepted, in the agent's own sequence.
    pub connected_seq: u64,
    /// When it last said `Active`. Zero means never.
    pub active_seq: u64,
    pub pinned: Vec<koru_agent_wire::SearchPin>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspacePidMatch {
    None,
    Unique(ConnectionId),
    Ambiguous,
}

/// One unique open workspace as the agent tray presents it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpenWorkspace {
    pub connection: ConnectionId,
    pub label: String,
}

#[derive(Default)]
pub struct Registry {
    entries: HashMap<ConnectionId, Shell>,
    /// One counter for both stamps, so an `Active` and a connection can always
    /// be ordered against each other.
    seq: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn tick(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// A shell said `Hello`. Adds an entry, or **updates** the one this
    /// connection already has.
    ///
    /// A second `Hello` on one connection is not an error and not a second
    /// shell: a client that re-introduces itself has changed its mind about
    /// what it is showing, which is what `WorkspaceChanged` is for and what a
    /// repeated `Hello` amounts to.
    pub fn hello(
        &mut self,
        connection: ConnectionId,
        pid: Option<u32>,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) {
        self.hello_named(connection, pid, profile, workspace, "", "");
    }

    pub fn hello_named(
        &mut self,
        connection: ConnectionId,
        pid: Option<u32>,
        profile: ProfileId,
        workspace: WorkspaceId,
        profile_name: &str,
        workspace_name: &str,
    ) {
        let connected_seq = match self.entries.get(&connection) {
            Some(existing) => existing.connected_seq,
            None => self.tick(),
        };
        let active_seq = self
            .entries
            .get(&connection)
            .map(|existing| existing.active_seq)
            .unwrap_or(0);
        let pinned = match self.entries.get(&connection) {
            Some(shell) => shell.pinned.clone(),
            None => Vec::new(),
        };
        self.entries.insert(
            connection,
            Shell {
                connection,
                pid,
                profile,
                workspace,
                profile_name: profile_name.to_owned(),
                workspace_name: workspace_name.to_owned(),
                connected_seq,
                active_seq,
                pinned,
            },
        );
    }

    pub fn set_pinned(
        &mut self,
        connection: ConnectionId,
        pinned: Vec<koru_agent_wire::SearchPin>,
    ) {
        if let Some(shell) = self.entries.get_mut(&connection) {
            shell.pinned = pinned;
        }
    }

    pub fn pinned_rows(
        &self,
    ) -> Vec<(
        String,
        koru_store_core::ids::DocumentId,
        koru_store_core::ids::WorkspaceId,
        bool,
    )> {
        let mut out = Vec::new();
        for shell in self.entries.values() {
            for pin in &shell.pinned {
                let Some(document) = koru_store_core::ids::DocumentId::parse(&pin.document) else {
                    continue;
                };
                let Some(workspace) = koru_store_core::ids::WorkspaceId::parse(&pin.workspace)
                else {
                    continue;
                };
                if out.iter().any(|(_, id, _, _)| *id == document) {
                    continue;
                }
                out.push((pin.title.clone(), document, workspace, pin.show_title));
            }
        }
        out
    }

    /// It is showing something else now. Silently ignored for a connection that
    /// never said `Hello` — a shell that skipped the introduction has told us
    /// nothing we can route on.
    pub fn workspace_changed(
        &mut self,
        connection: ConnectionId,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) {
        self.workspace_changed_named(connection, profile, workspace, "", "");
    }

    pub fn workspace_changed_named(
        &mut self,
        connection: ConnectionId,
        profile: ProfileId,
        workspace: WorkspaceId,
        profile_name: &str,
        workspace_name: &str,
    ) {
        if let Some(shell) = self.entries.get_mut(&connection) {
            shell.profile = profile;
            shell.workspace = workspace;
            shell.profile_name = profile_name.to_owned();
            shell.workspace_name = workspace_name.to_owned();
        }
    }

    /// Attach the transport-authenticated pid when its accept event arrives.
    ///
    /// Ordinarily this precedes `Hello`; accepting either order makes the
    /// foreground capability robust to the rendezvous and reader threads
    /// reaching the event loop in the same scheduling slice.
    pub fn pid_arrived(&mut self, connection: ConnectionId, pid: Option<u32>) {
        if let Some(shell) = self.entries.get_mut(&connection) {
            shell.pid = pid;
        }
    }

    /// It was just brought to the front.
    pub fn active(&mut self, connection: ConnectionId) {
        let seq = self.tick();
        if let Some(shell) = self.entries.get_mut(&connection) {
            shell.active_seq = seq;
        }
    }

    /// The socket ended. Clean close, crash or kill — all the same.
    pub fn gone(&mut self, connection: ConnectionId) -> bool {
        self.entries.remove(&connection).is_some()
    }

    /// Registered workspace windows — the `Status.shells` count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The shell that should be given a result in this workspace.
    ///
    /// **Highest `active_seq`, then highest `connected_seq`.** "The window I was
    /// just in" wins, which is the answer a person expects; the second key
    /// settles two windows that have never been focused, and ordering by pid
    /// would get it wrong after a restart because pids are reused.
    pub fn route(&self, profile: ProfileId, workspace: WorkspaceId) -> Option<&Shell> {
        self.entries
            .values()
            .filter(|shell| shell.profile == profile && shell.workspace == workspace)
            .max_by_key(|shell| (shell.active_seq, shell.connected_seq))
    }

    /// The most recently active shell holding any workspace of this profile.
    ///
    /// Used when a search result wants to *reuse* a window rather than open a
    /// second one: the shell switches workspace in place. Exact-workspace
    /// routing still wins when something already shows the target.
    pub fn route_profile(&self, profile: ProfileId) -> Option<&Shell> {
        self.entries
            .values()
            .filter(|shell| shell.profile == profile)
            .max_by_key(|shell| (shell.active_seq, shell.connected_seq))
    }

    pub fn shell(&self, connection: ConnectionId) -> Option<&Shell> {
        self.entries.get(&connection)
    }

    /// The authenticated Workspace process matching an OS foreground pid.
    /// Multiple windows claiming the same process fail closed.
    pub fn workspace_for_pid(&self, pid: u32) -> WorkspacePidMatch {
        let mut matching = self
            .entries
            .values()
            .filter(|shell| shell.pid == Some(pid))
            .map(|shell| shell.connection);
        let Some(first) = matching.next() else {
            return WorkspacePidMatch::None;
        };
        if matching.next().is_some() {
            WorkspacePidMatch::Ambiguous
        } else {
            WorkspacePidMatch::Unique(first)
        }
    }

    /// The Koru window the person used most recently, independent of profile
    /// or workspace. Used for an ask that names no destination of its own: a
    /// tray Agents row, or a chat to open as a tab.
    pub fn most_recent(&self) -> Option<&Shell> {
        self.shells()
            .into_iter()
            .max_by_key(|shell| (shell.active_seq, shell.connected_seq))
    }

    /// The open workspace rows for the agent tray, with duplicate windows on
    /// one workspace collapsed to the same winner used by result routing.
    pub fn open_workspaces(&self) -> Vec<OpenWorkspace> {
        let mut unique: HashMap<(ProfileId, WorkspaceId), &Shell> = HashMap::new();
        for shell in self.entries.values() {
            unique
                .entry((shell.profile, shell.workspace))
                .and_modify(|current| {
                    if (shell.active_seq, shell.connected_seq)
                        > (current.active_seq, current.connected_seq)
                    {
                        *current = shell;
                    }
                })
                .or_insert(shell);
        }
        let mut rows: Vec<_> = unique
            .into_values()
            .map(|shell| OpenWorkspace {
                connection: shell.connection,
                label: format!(
                    "{} — {}",
                    display_name(&shell.workspace_name, "Untitled workspace"),
                    display_name(&shell.profile_name, "Untitled profile")
                ),
            })
            .collect();
        rows.sort_by(|left, right| {
            left.label
                .to_lowercase()
                .cmp(&right.label.to_lowercase())
                .then_with(|| left.connection.cmp(&right.connection))
        });
        rows
    }

    /// Every shell, for a diagnostic. Ordered, so a log is comparable between
    /// runs rather than being whatever the hash map felt like.
    pub fn shells(&self) -> Vec<&Shell> {
        let mut all: Vec<&Shell> = self.entries.values().collect();
        all.sort_by_key(|shell| shell.connection);
        all
    }
}

fn display_name<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests;
