// SPDX-License-Identifier: MPL-2.0

//! The run's own MCP endpoint and what it registered, through the gateway.
//!
//! Two ports over one `koru-api-client`, and between them they are what
//! makes a delegated run able to hand a result back:
//!
//! - [`GatewayAuthority`] issues the run's authority before the provider
//!   exists (`POST /v1/artifacts/authorities`), advances the generation for
//!   a second attempt so the first attempt's token dies, and revokes when
//!   the run ends. What it answers with is the launch the adapter renders:
//!   the endpoint, the bearer for **this** attempt, and the six tool names
//!   the endpoint publishes.
//! - [`GatewayRegistrations`] reads `GET /v1/artifacts/runs/{run}/versions`
//!   at each turn boundary and maps what is new to a [`ResultReference`].
//!   That answer is what decides D29 — a turn that registered nothing needs
//!   attention, a turn that registered something completes the run — and
//!   what fills the Result tab.
//!
//! **The bearer is not stored here.** It crosses once, in the issue reply,
//! goes into the [`DelegationLaunch`] and out to the session host, and this
//! module keeps no copy: a lost reply is answered by advancing the
//! generation, never by asking for the same token again (ADR-348). Nothing
//! in this file logs a token, a title or a path.

use std::collections::HashMap;

use koru_api_client::Client;
use koru_artifact_wire::authority::{
    AdvanceGenerationRequest, IssueAuthorityRequest, RunWorkspace,
};
use koru_delegate_core::{
    AgentTurnId, ArtifactTitle, ArtifactVersionId, DeviceId, ResultReference, RunAttemptId, RunId,
    WorkspaceId,
};
mod projection;
pub(crate) use projection::AuthorityProjection;

use koru_delegate_runtime::{
    ArtifactRegistrations, McpEndpoints, McpLaunch, Port, PortError, PortResult,
};
use koru_session_protocol::{DelegationLaunch, McpToolAllowlist};

/// The six tools `/v1/runs/{run}/mcp` publishes, in the spelling
/// `koru_api_gateway_core::run_mcp::tools` uses and P1.12's preamble names.
/// A seventh name here would be a tool the model is offered and the
/// endpoint refuses by name; a missing one is a tool the preamble tells it
/// to use and the launch forbids.
pub const RUN_TOOLS: [&str; 6] = [
    "attach_to_run",
    "create",
    "create_version",
    "get",
    "read",
    "search",
];

/// The tool allowlist every delegated launch carries. Built once; the set
/// is fixed and small enough that a failure here is a programming error.
fn allowlist() -> McpToolAllowlist {
    McpToolAllowlist::new(
        RUN_TOOLS
            .iter()
            .map(|name| name.parse().expect("a run tool name is a valid MCP name")),
    )
    .expect("six names are under the allowlist bound")
}

/// The Bot endpoint's native document and own-Memory tools. Keep this exact
/// vocabulary aligned with the gateway's catalog test; it grants no page access.
const BOT_RUN_TOOLS: [&str; 9] = [
    "search_documents",
    "read_document",
    "create_document",
    "edit_document",
    "list_memory",
    "read_memory",
    "remember_memory",
    "correct_memory",
    "forget_memory",
];

fn launch_tools(
    authority: &koru_delegate_core::RunToolAuthority,
    ordinary: &McpToolAllowlist,
) -> McpToolAllowlist {
    if matches!(authority, koru_delegate_core::RunToolAuthority::Bot { .. }) {
        McpToolAllowlist::new(
            BOT_RUN_TOOLS
                .into_iter()
                .map(|name| name.parse().expect("native tool name")),
        )
        .expect("nine native tools")
    } else {
        ordinary.clone()
    }
}

fn refusal(operation: &'static str, failure: &koru_artifact_wire::Failure) -> PortError {
    PortError::new(Port::Mcp, failure.code.clone(), operation)
}

pub struct GatewayAuthority {
    client: Client,
    /// `KORU_DELEGATE_MCP_ENDPOINT`, when a person set it: the base the
    /// endpoint URL is rewritten onto. Unset, the gateway's own answer
    /// stands — it knows the endpoint it published, and this worker does
    /// not.
    base_override: Option<String>,
    tools: McpToolAllowlist,
    projection: AuthorityProjection,
}

impl GatewayAuthority {
    pub(crate) fn with_projection(
        client: Client,
        base_override: Option<String>,
        projection: AuthorityProjection,
    ) -> Self {
        Self {
            client,
            base_override: base_override.map(|base| base.trim_end_matches('/').to_owned()),
            tools: allowlist(),
            projection,
        }
    }

    fn endpoint(&self, run: RunId, answered: String) -> String {
        match &self.base_override {
            Some(base) => format!("{base}/v1/runs/{}/mcp", run.to_storage()),
            None => answered,
        }
    }
}

impl McpEndpoints for GatewayAuthority {
    fn publish_authority(
        &mut self,
        run: RunId,
        generation: koru_delegate_core::AttemptGeneration,
        authority: koru_delegate_core::RunToolAuthority,
    ) -> PortResult<()> {
        self.projection
            .publish(run, generation, authority)
            .map_err(|e| PortError::new(Port::Mcp, e.code, "Run classification"))
    }
    fn launch(&mut self, launch: McpLaunch<'_>) -> PortResult<DelegationLaunch> {
        let run = launch.run;
        let tools = launch_tools(&launch.authority, &self.tools);
        let run_workspace = RunWorkspace::new(launch.run_workspace)
            .map_err(|error| PortError::new(Port::Mcp, error.code(), "the run's working folder"))?;
        let artifacts = self.client.artifacts();
        // Issue, or advance if this run already has an authority. The
        // second is the ordinary path of a second attempt (§7.6) and also
        // the honest answer to a reply this worker lost: the token that was
        // current is retired, so nothing that survived the loss can still
        // publish.
        let issued = match artifacts.issue_run_authority(&IssueAuthorityRequest {
            run,
            workspace: launch.workspace,
            generation: launch.generation,
            grants: launch.grants.clone(),
            run_workspace,
        }) {
            Ok(issued) => issued,
            Err(failure) if failure.code == "authority_exists" => artifacts
                .advance_run_generation(
                    run,
                    &AdvanceGenerationRequest {
                        generation: launch.generation,
                    },
                )
                .map_err(|failure| refusal("advance a run generation", &failure))?,
            Err(failure) => return Err(refusal("issue a run authority", &failure)),
        };
        let endpoint = self.endpoint(run, issued.url);
        let delegation = DelegationLaunch {
            model: None,
            mcp_endpoint: endpoint
                .parse()
                .map_err(|_| PortError::new(Port::Mcp, "invalid_endpoint", "the endpoint URL"))?,
            credential: Some(issued.token.parse().map_err(|_| {
                PortError::new(Port::Mcp, "invalid_credential", "the endpoint bearer")
            })?),
            tools,
            resume: None,
        };
        // Nothing after publication can fail locally; failed construction does
        // not remove an unrelated newer generation's classification.
        self.publish_authority(run, launch.generation, launch.authority)?;
        Ok(delegation)
    }

    /// Where the run worked, from the durable record — the only place this
    /// device wrote it down. `None` for a run that never started and for
    /// one this device did not run: `authority_not_found` is an answer, not
    /// a failure, because the caller is deleting a run that may have had
    /// nothing here at all.
    fn run_workspace(&mut self, run: RunId) -> PortResult<Option<std::path::PathBuf>> {
        match self.client.artifacts().run_authority_workspace(run) {
            Ok(answer) => Ok(Some(answer.run_workspace.as_path().to_path_buf())),
            Err(failure) if failure.code == "authority_not_found" => Ok(None),
            Err(failure) => Err(refusal("read a run's working folder", &failure)),
        }
    }

    /// The run was deleted. Unlike [`Self::revoke`] this is **not** best
    /// effort: it is the step that removes the person's own path from the
    /// store, and a delete that silently left it there would be a delete
    /// that did not happen.
    fn forget(&mut self, run: RunId) -> PortResult<()> {
        self.projection.remove(run, None);
        self.client
            .artifacts()
            .forget_run_authority(run)
            .map(|_| ())
            .map_err(|failure| refusal("forget a run's authority", &failure))
    }

    fn revoke(&mut self, run: RunId) {
        self.projection.remove(run, None);
        if let Err(failure) = self.client.artifacts().revoke_run_authority(run) {
            // Best effort, and said so in the port's doc comment: a
            // revocation that did not land leaves an authority whose run is
            // over, and the next call it serves is refused on the record
            // anyway. Never a reason to fail a finished run.
            tracing::warn!(
                target: "koru::delegate",
                run = %run,
                code = %failure.code,
                "the run's authority was not revoked"
            );
        }
    }
}

#[cfg(test)]
mod tests;

/// What a run has registered, and what this device has already told the
/// orchestrator about.
///
/// The gateway answers with *everything* the run ever registered; the port
/// is asked *what is new*. The difference is this map: one set of version
/// ids per run, added to as they are reported. It is a working note, not a
/// record — the Run document is the record, and the orchestrator checks it
/// too — so a restart losing this map costs a re-ask, not a duplicate.
pub struct GatewayRegistrations {
    client: Client,
    device: DeviceId,
    reported: HashMap<RunId, Vec<ArtifactVersionId>>,
}

impl GatewayRegistrations {
    pub fn new(client: Client, device: DeviceId) -> Self {
        Self {
            client,
            device,
            reported: HashMap::new(),
        }
    }
}

impl ArtifactRegistrations for GatewayRegistrations {
    fn registered(
        &mut self,
        run: RunId,
        _attempt: RunAttemptId,
        _turn: Option<AgentTurnId>,
    ) -> Vec<ResultReference> {
        let artifacts = self.client.artifacts();
        let versions = match artifacts.run_versions(run) {
            Ok(answer) => answer.versions,
            Err(failure) => {
                // A turn boundary the gateway could not answer counts
                // nothing, which is D29's needing-attention — the safe
                // reading, and a person can still see the results in the
                // Result tab.
                tracing::warn!(
                    target: "koru::delegate",
                    run = %run,
                    code = %failure.code,
                    "the run's registrations could not be read"
                );
                return Vec::new();
            }
        };
        let seen = self.reported.entry(run).or_default();
        let mut fresh = Vec::new();
        for version in versions {
            if seen.contains(&version.id) {
                continue;
            }
            // The title lives on the family, not the version, so a result a
            // person will read by name costs one more read. A family that
            // cannot be read is still a result: it is named by nothing
            // rather than dropped.
            let title = artifacts
                .family(version.family)
                .ok()
                .map(|answer| answer.family.title)
                .unwrap_or_else(|| ArtifactTitle::new("Result").expect("a literal, bounded title"));
            seen.push(version.id);
            fresh.push(ResultReference {
                artifact: version.id,
                held_by: self.device,
                title,
                size_bytes: version.size_bytes,
                registered_at_ms: version.created_at_ms,
            });
        }
        fresh
    }

    /// **The person asked for the run's results to go with it.**
    ///
    /// Every family the run created is retired — marked, never erased, the
    /// way this store retires everything: a version is immutable and
    /// something else may already point at one. The column stops offering
    /// them, which is what *delete the artifact* means here.
    ///
    /// Retired families are asked for too, so a second empty after a lost
    /// answer retires nothing again rather than failing; the count is what
    /// this call actually retired.
    fn retire_results(&mut self, run: RunId, workspace: WorkspaceId) -> PortResult<u32> {
        let artifacts = self.client.artifacts();
        let families = artifacts
            .list_families(workspace, false)
            .map_err(|failure| refusal("list a workspace's results", &failure))?
            .families;
        let mut retired = 0;
        for family in families
            .into_iter()
            .filter(|family| family.created_by_run == Some(run))
        {
            artifacts
                .retire_family(family.id)
                .map_err(|failure| refusal("retire a result", &failure))?;
            retired += 1;
        }
        Ok(retired)
    }
}
