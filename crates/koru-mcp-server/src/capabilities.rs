// SPDX-License-Identifier: MPL-2.0

//! Which tools a host serves at all.
//!
//! The shared catalog is every tool this crate implements. A host that is not
//! the desktop — the hosted MCP server at `mcp.koru.do` — serves only the
//! tools its rung has built: files, versions, chats and `update_task` are not
//! in rung 1, and a hosted connection must never reach them by naming them.
//!
//! **One set, for the catalog and for admission.** [`HostCapabilities`] rides
//! in the [`RequestContext`](crate::RequestContext), and the executor checks
//! it **first** — before scope policy, before workspace authority, before any
//! backend work — so a tool outside it is refused as `tool_not_available`
//! whatever the caller's scopes. [`HostCapabilities::catalog`] publishes the
//! same set, so a host cannot list a tool it would refuse, or refuse one it
//! listed.
//!
//! **Toolsets are something else, and compose on top.** A bridge's
//! `--toolsets` ([`crate::Toolsets`]) narrows what one stdio bridge *lists*, to
//! save an agent's context; every toolset's tool is still a tool that host
//! serves, and calling one that is off answers `toolset_disabled` with the
//! flag that turns it on. Capabilities say what a host *can* do. The local
//! bridge is [`HostCapabilities::All`], so its toolsets work exactly as
//! before; a hosted catalog is not toolset-shaped — rung 1 is part of `docs`
//! and part of `tasks` — which is why it is a set of tools and not a set of
//! toolsets.
//!
//! **Claims are hosted only** ([`crate::hosted`]): `claim_document` and
//! `release_document` are in rung 1 and in no shared router, so
//! [`HostCapabilities::All`] — every local host — refuses them by name, and a
//! hosted catalog adds them, with `claim_ids` on `edit_document` and
//! `extract_to_document`. The local catalog is the shared one, unchanged.

use serde_json::json;

use crate::hosted::{self, HOSTED_ONLY_TOOLS};
use crate::{ToolError, WorkspaceTool, workspace_tool_catalog};

/// The tools a host serves. Local hosts — the desktop bridge and Koru AI —
/// serve [`All`](HostCapabilities::All).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostCapabilities {
    /// Every tool in the shared catalog.
    #[default]
    All,
    /// Only these, by name.
    Tools(&'static [&'static str]),
}

/// Rung 1 of Cloud MCP (TECHNICAL §1): read and edit documents and tasks,
/// and claim documents. Tasks change through `edit_document`, so
/// `update_task` is not here.
pub const RUNG1_TOOLS: &[&str] = &[
    "get_documentation",
    "list_workspaces",
    "list_documents",
    "search_documents",
    "read_document",
    "create_document",
    "edit_document",
    "create_image_upload",
    "add_image",
    "replace_image",
    "extract_to_document",
    "list_tasks",
    hosted::CLAIM_DOCUMENT,
    hosted::RELEASE_DOCUMENT,
];

/// Rung 1's reads alone: what a hosted host serves until its mutation
/// outcomes are built (Cloud MCP M1), and what a read-only connection is ever
/// shown.
pub const RUNG1_READ_TOOLS: &[&str] = &[
    "get_documentation",
    "list_workspaces",
    "list_documents",
    "search_documents",
    "read_document",
    "list_tasks",
];

impl HostCapabilities {
    /// The hosted rung-1 catalog.
    pub const fn rung1() -> Self {
        Self::Tools(RUNG1_TOOLS)
    }

    /// The hosted rung-1 reads.
    pub const fn rung1_reads() -> Self {
        Self::Tools(RUNG1_READ_TOOLS)
    }

    /// Whether this host is a device — the desktop bridge, Koru AI — rather
    /// than a hosted replica. Every local host serves
    /// [`All`](HostCapabilities::All); a host that serves a subset is hosted.
    ///
    /// **What a device keeps that a replica does not** (TECHNICAL §5.5): the
    /// `mcp_before`/`mcp_after` history captures, because history is
    /// device-local and a replica is in memory, so hosted answers carry no
    /// `history` field; and the task log's delivery (`tasks.deliver`), which a
    /// replica has nowhere durable to keep. Neither is an effect. A hosted
    /// `create_document` also always answers its `idempotency_key`.
    pub fn is_device(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether this host serves `tool`. A local host serves every shared
    /// tool and none of the hosted-only ones.
    pub fn allows(&self, tool: &str) -> bool {
        match self {
            Self::All => !HOSTED_ONLY_TOOLS.contains(&tool),
            Self::Tools(tools) => tools.contains(&tool),
        }
    }

    /// The published catalog. A local host's is the shared catalog, unchanged.
    /// A hosted one's is the shared catalog's tools it serves, in that order
    /// and with those schemas, `claim_ids` added to `edit_document` and
    /// `extract_to_document`, then the claim tools it serves.
    pub fn catalog(&self) -> Vec<WorkspaceTool> {
        let shared = workspace_tool_catalog()
            .into_iter()
            .filter(|tool| self.allows(&tool.name));
        if self.is_device() {
            return shared.collect();
        }
        shared
            .map(hosted::hosted_tool)
            .chain(
                hosted::claim_tools()
                    .into_iter()
                    .filter(|tool| self.allows(&tool.name)),
            )
            .collect()
    }

    /// The refusal for a tool this host does not serve, before anything else
    /// is asked of the call.
    pub(crate) fn refusal(&self, tool: &str) -> Option<ToolError> {
        (!self.allows(tool)).then(|| {
            ToolError {
                code: "tool_not_available".into(),
                message: format!("{tool} is not available on this Koru host"),
                details: None,
            }
            .with_details(json!({ "tool": tool }))
        })
    }
}

#[cfg(test)]
mod tests;
