// SPDX-License-Identifier: MPL-2.0

//! `client.artifacts()` — the Artifact Store through the gateway.
//!
//! Blocking calls over the gateway session this crate already holds, the
//! way the AI namespace does it: the bearer, its renewal and the loopback
//! endpoint are the `Client`'s, and a call here adds only the path and the
//! body. Bytes are the one exception, and they are still not this module's:
//! a version's content is *uploaded* through the Store namespace's
//! `put_blob` (the object path, streamed, with the Store's own limit) and
//! *read* through [`ArtifactsClient::content`], which the gateway forwards
//! to that same blob store by hash.

use bytes::Bytes;
use koru_artifact_wire::authority::{
    AdvanceGenerationRequest, AuthorityView, ForgetAuthorityResponse, IssueAuthorityRequest,
    IssuedAuthority, RunVersionsResponse, RunWorkspaceResponse,
};
use koru_artifact_wire::run_folder::{RegisterFileRequest, RegisteredFile, UnregisteredResponse};
use koru_artifact_wire::{
    ArtifactFamily, ArtifactFamilyId, ArtifactVersion, ArtifactVersionId, CreateFamilyRequest,
    Failure, FamilyResponse, LinkRunRequest, ListFamiliesResponse, Provenance,
    RegisterVersionRequest, RegisterVersionResponse, RunId, RunLink, WorkspaceId,
};

use crate::{Client, Error};

pub type ArtifactsResult<T> = std::result::Result<T, Failure>;

pub struct ArtifactsClient<'a> {
    pub(crate) client: &'a Client,
}

impl ArtifactsClient<'_> {
    /// Whether the gateway published the `artifacts` capability — whether
    /// `artifacts.db` opened. When false every call below answers
    /// `artifacts_unavailable`.
    pub fn available(&self) -> bool {
        self.client
            .capabilities
            .iter()
            .any(|value| value == koru_artifact_wire::CAPABILITY)
    }

    pub fn list_families(
        &self,
        workspace: WorkspaceId,
        include_retired: bool,
    ) -> ArtifactsResult<ListFamiliesResponse> {
        self.json(
            reqwest::Method::GET,
            format!(
                "/v1/artifacts/families?workspace={}&include_retired={include_retired}",
                workspace.to_storage()
            ),
            None,
        )
    }

    pub fn create_family(&self, request: &CreateFamilyRequest) -> ArtifactsResult<ArtifactFamily> {
        self.json(
            reqwest::Method::POST,
            "/v1/artifacts/families".into(),
            Some(json_body(request)?),
        )
    }

    pub fn family(&self, family: ArtifactFamilyId) -> ArtifactsResult<FamilyResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/families/{}", family.to_storage()),
            None,
        )
    }

    pub fn retire_family(&self, family: ArtifactFamilyId) -> ArtifactsResult<ArtifactFamily> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/families/{}/retire", family.to_storage()),
            None,
        )
    }

    /// Register metadata against a blob already in the family's workspace
    /// — see `client.store().put_blob` for getting it there.
    pub fn register_version(
        &self,
        family: ArtifactFamilyId,
        request: &RegisterVersionRequest,
    ) -> ArtifactsResult<RegisterVersionResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/families/{}/versions", family.to_storage()),
            Some(json_body(request)?),
        )
    }

    pub fn version(&self, version: ArtifactVersionId) -> ArtifactsResult<ArtifactVersion> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/versions/{}", version.to_storage()),
            None,
        )
    }

    pub fn provenance(&self, version: ArtifactVersionId) -> ArtifactsResult<Provenance> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/versions/{}/provenance", version.to_storage()),
            None,
        )
    }

    pub fn link_run(
        &self,
        version: ArtifactVersionId,
        request: &LinkRunRequest,
    ) -> ArtifactsResult<RunLink> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/versions/{}/runs", version.to_storage()),
            Some(json_body(request)?),
        )
    }

    // ------------------------------------------------------ run authority

    /// Issue a run's authority (ROADMAP §7.3): the endpoint the provider is
    /// handed and the bearer for this attempt. The bearer is shown here
    /// and nowhere else; keep it beside the attempt, and advance the
    /// generation rather than asking again after a lost reply.
    pub fn issue_run_authority(
        &self,
        request: &IssueAuthorityRequest,
    ) -> ArtifactsResult<IssuedAuthority> {
        self.json(
            reqwest::Method::POST,
            "/v1/artifacts/authorities".into(),
            Some(json_body(request)?),
        )
    }

    /// The run's record without its token or its path.
    pub fn run_authority(&self, run: RunId) -> ArtifactsResult<AuthorityView> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/authorities/{}", run.to_storage()),
            None,
        )
    }

    /// A fresh attempt: a strictly newer generation under a fresh token,
    /// the same endpoint. The previous token answers `stale_generation`
    /// from then on.
    pub fn advance_run_generation(
        &self,
        run: RunId,
        request: &AdvanceGenerationRequest,
    ) -> ArtifactsResult<IssuedAuthority> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/authorities/{}/generation", run.to_storage()),
            Some(json_body(request)?),
        )
    }

    /// Revoke the run's authority: every later call at its endpoint
    /// answers `authority_revoked`. Idempotent.
    pub fn revoke_run_authority(&self, run: RunId) -> ArtifactsResult<AuthorityView> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/authorities/{}/revoke", run.to_storage()),
            None,
        )
    }

    /// **Where the run worked**, as this device resolved it when it
    /// started. The one read that carries a path out of the Artifact Store,
    /// for the one caller entitled to it: emptying the trash, which has to
    /// remove that folder when Koru made it.
    pub fn run_authority_workspace(&self, run: RunId) -> ArtifactsResult<RunWorkspaceResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/authorities/{}/workspace", run.to_storage()),
            None,
        )
    }

    /// **The run was deleted**: its authority, grants and tokens go, and a
    /// row saying the run was deleted takes their place. Idempotent, and
    /// not an error for a run that never had an authority.
    pub fn forget_run_authority(&self, run: RunId) -> ArtifactsResult<ForgetAuthorityResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/authorities/{}/forget", run.to_storage()),
            None,
        )
    }

    /// The versions a run has registered, oldest first — what the
    /// orchestrator reads at a turn boundary (D29).
    pub fn run_versions(&self, run: RunId) -> ArtifactsResult<RunVersionsResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/artifacts/runs/{}/versions", run.to_storage()),
            None,
        )
    }

    // ------------------------------------------------- a run's folder

    /// What the workspace's runs wrote and never registered (D19, D37),
    /// beside how much each has already delivered (D29). Newest run first;
    /// a link is never listed and the scan is bounded, which is what
    /// `truncated` says.
    pub fn unregistered(&self, workspace: WorkspaceId) -> ArtifactsResult<UnregisteredResponse> {
        self.json(
            reqwest::Method::GET,
            format!(
                "/v1/artifacts/unregistered?workspace={}",
                workspace.to_storage()
            ),
            None,
        )
    }

    /// D19's one click: adopt a file the run left in its own
    /// `.artifacts/<RunId>/` folder as a result. The path goes through the
    /// same register path rule the run's own tools obey, and a refusal
    /// carries the same stable code.
    pub fn register_file(
        &self,
        run: RunId,
        request: &RegisterFileRequest,
    ) -> ArtifactsResult<RegisteredFile> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/artifacts/runs/{}/register", run.to_storage()),
            Some(json_body(request)?),
        )
    }

    /// The version's bytes, as the Store filed them under its hash.
    pub fn content(&self, version: ArtifactVersionId) -> ArtifactsResult<Vec<u8>> {
        let response = checked(self.execute(
            reqwest::Method::GET,
            format!("/v1/artifacts/versions/{}/content", version.to_storage()),
            None,
        )?)?;
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|_| protocol_failure("The artifact content body could not be read."))
    }

    /// Read a bounded format without trusting either metadata or an HTTP
    /// length hint. The stream consumes at most `limit + 1` body bytes.
    pub fn content_bounded(
        &self,
        version: ArtifactVersionId,
        limit: usize,
    ) -> ArtifactsResult<Vec<u8>> {
        let response = checked(self.execute(
            reqwest::Method::GET,
            format!("/v1/artifacts/versions/{}/content", version.to_storage()),
            None,
        )?)?;
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(content_too_large());
        }
        read_bounded(response, limit)
    }

    fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<Bytes>,
    ) -> ArtifactsResult<T> {
        checked(self.execute(method, path, body)?)?
            .json()
            .map_err(|_| protocol_failure("The artifact response body was invalid."))
    }

    fn execute(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<Bytes>,
    ) -> ArtifactsResult<reqwest::blocking::Response> {
        let session = self.client.gateway.as_ref().ok_or_else(|| {
            Failure::new(
                "artifacts_unavailable",
                "This client was not created from a Koru API gateway session.",
                true,
            )
        })?;
        let current = session.current().map_err(gateway_failure)?;
        let http = self
            .client
            .http
            .as_ref()
            .expect("a gateway session owns its HTTP adapters");
        let send = |current: &koru_api_discovery::GatewayHello| {
            let mut request = http
                .request(method.clone(), format!("{}{}", current.api_endpoint, path))
                .bearer_auth(current.session.expose());
            if let Some(body) = &body {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone());
            }
            request.send()
        };
        match send(&current) {
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                let renewed = session
                    .refresh_after_rejection(current.session.expose())
                    .map_err(gateway_failure)?;
                if renewed.session.expose() == current.session.expose() {
                    Ok(response)
                } else {
                    send(&renewed).map_err(|_| transport_failure())
                }
            }
            Ok(response) => Ok(response),
            Err(_) => {
                let renewed = session.refresh("unreachable").map_err(gateway_failure)?;
                send(&renewed).map_err(|_| transport_failure())
            }
        }
    }
}

fn content_too_large() -> Failure {
    Failure::new(
        "payload_too_large",
        "The artifact exceeds this format's byte limit.",
        false,
    )
}

fn read_bounded(reader: impl std::io::Read, limit: usize) -> ArtifactsResult<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| protocol_failure("The artifact content body could not be read."))?;
    if bytes.len() > limit {
        return Err(content_too_large());
    }
    Ok(bytes)
}

#[cfg(test)]
mod bounded_tests;

fn json_body<T: serde::Serialize>(value: &T) -> ArtifactsResult<Bytes> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|_| protocol_failure("The artifact request could not be encoded."))
}

fn checked(response: reqwest::blocking::Response) -> ArtifactsResult<reqwest::blocking::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    // An error is metadata too. A failed bounded content read must not
    // allocate an unbounded error body before reaching its format limit.
    match read_bounded(response, koru_artifact_wire::METADATA_BODY_LIMIT)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Failure>(&bytes).ok())
    {
        Some(failure) => Err(failure),
        None => Err(Failure::new(
            if status == reqwest::StatusCode::UNAUTHORIZED {
                "unauthenticated"
            } else {
                "gateway_failure"
            },
            "The Koru API gateway returned an invalid error response.",
            status.is_server_error(),
        )),
    }
}

fn protocol_failure(message: &str) -> Failure {
    Failure::new("invalid_response", message, true)
}

fn gateway_failure(error: Error) -> Failure {
    Failure::new("gateway_unavailable", error.to_string(), true)
}

fn transport_failure() -> Failure {
    Failure::new(
        "gateway_unavailable",
        "The Koru API gateway connection ended.",
        true,
    )
}
