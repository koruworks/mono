// SPDX-License-Identifier: MPL-2.0

//! The five control routes, from the client's side.
//!
//! ```text
//! GET    /v1/cloud/workspaces/{workspace}/blobs/{hash}
//! POST   /v1/cloud/workspaces/{workspace}/blobs/{hash}/uploads
//! POST   /v1/cloud/workspaces/{workspace}/blob-uploads/{upload}/capability
//! DELETE /v1/cloud/workspaces/{workspace}/blob-uploads/{upload}
//! POST   /v1/cloud/workspaces/{workspace}/blobs/{hash}/read-capability
//! ```
//!
//! Every body comes from [`koru_cloud_blob::wire`] rather than being spelled
//! again here, so a field this client reads and a field that service writes
//! cannot drift apart.
//!
//! **Two vocabularies reach this client and it keeps them apart.** Anything
//! about the *blob* answers the contract's §7 registry and is carried through
//! as a [`BlobCode`]; anything about the *account* answers the account API's
//! codes, which this client does not branch on at all — it has one question
//! about them ("am I admitted?") and one answer ([`TransferError::NoAccess`]),
//! because the remedy is the account owner's and never a retry from here.

use koru_cloud_blob::{
    BlobCode,
    wire::{AllocateRequestV1, AllocationV1, BlobErrorV1, BlobStatusV1, ReadCapabilityV1},
};
use koru_device::cloud::CloudId;
use koru_store_core::{WorkspaceId, errors::Code, model::BlobHash};

use crate::ports::{CloudAccessFacts, PortFuture, TransferError};

/// How long one control request may take before it is called unreachable.
///
/// The control plane answers from a database and mints one signature; it never
/// moves bytes. Ten seconds is the account client's own timeout, and using the
/// same number means one answer to "how long does this device wait on the API"
/// rather than two that drift.
pub const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The largest control body this client will read. Every one of them is a
/// small JSON object; bytes go to the object plane and never here.
pub const MAX_CONTROL_BODY: usize = 64 * 1024;

/// The control plane, as a transfer needs it.
pub trait BlobControlPlane: Send + Sync {
    fn status(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<BlobStatusV1, TransferError>>;

    fn allocate(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
        request: AllocateRequestV1,
    ) -> PortFuture<'_, Result<AllocationV1, TransferError>>;

    /// A fresh capability for the **same** session. Never a second upload and
    /// never a new locator.
    fn renew(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        upload: CloudId,
    ) -> PortFuture<'_, Result<AllocationV1, TransferError>>;

    fn abort(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        upload: CloudId,
    ) -> PortFuture<'_, Result<(), TransferError>>;

    fn read_capability(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<ReadCapabilityV1, TransferError>>;
}

/// The real one.
pub struct HttpControlPlane {
    client: reqwest::Client,
    internal_token: Option<zeroize::Zeroizing<String>>,
}

impl HttpControlPlane {
    pub fn new() -> Result<Self, TransferError> {
        let client = reqwest::Client::builder()
            // **No redirects.** A control plane that answered `302` to
            // somewhere else would be handed the account bearer token, which
            // is the one credential that opens every Workspace of an account.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONTROL_TIMEOUT)
            .timeout(CONTROL_TIMEOUT)
            .build()
            .map_err(|_| TransferError::Unreachable)?;
        Ok(Self {
            client,
            internal_token: None,
        })
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            internal_token: None,
        }
    }

    /// The MCP worker calls the same control routes through the API's private
    /// listener. Its service token is required in addition to the user's MCP
    /// bearer and is never sent to the object worker.
    pub fn with_internal_token(mut self, token: zeroize::Zeroizing<String>) -> Self {
        self.internal_token = Some(token);
        self
    }

    fn url(access: &CloudAccessFacts, path: &str) -> Result<url::Url, TransferError> {
        access
            .api_origin
            .join(path)
            .map_err(|_| TransferError::Local(Code::InvalidRequest))
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        bearer: &str,
    ) -> Result<T, TransferError> {
        let mut request = request
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(token) = &self.internal_token {
            request = request.header(
                koru_account_wire::app_connections::INTERNAL_TOKEN_HEADER,
                token.as_str(),
            );
        }
        let response = request
            .send()
            .await
            .map_err(|_| TransferError::Unreachable)?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|_| TransferError::Unreachable)?;
        if body.len() > MAX_CONTROL_BODY {
            return Err(TransferError::Unreachable);
        }
        if (200..300).contains(&status) {
            return serde_json::from_slice(&body).map_err(|_| TransferError::Unreachable);
        }
        Err(refusal(status, &body))
    }
}

/// One refusal, in one place, so five routes cannot disagree about what a
/// status means.
fn refusal(status: u16, body: &[u8]) -> TransferError {
    if let Ok(error) = serde_json::from_slice::<BlobErrorV1>(body) {
        return TransferError::Refused(error.code);
    }
    match status {
        // The account is not admitted for this Workspace right now — an
        // expired token, a revoked membership, a Workspace this principal does
        // not qualify for. One answer for all of them on purpose: the remedy
        // belongs to whoever owns the account, and none of them is a retry.
        401 | 403 | 404 => TransferError::NoAccess,
        // Retryable by the contract's own word, and a rate limit is the same
        // shape of answer.
        429 | 500..=599 => TransferError::Refused(BlobCode::StorageUnavailable),
        // A request this client made that the service will not accept. About
        // this blob and not about the session, so its neighbours carry on.
        _ => TransferError::Local(Code::InvalidRequest),
    }
}

/// `{hash}` is the canonical `b3:<hex>` spelling, percent-encoded in the path.
/// The `b3-<hex>` form is a filename and never a path segment (D248), and the
/// colon is the character that makes the encoding matter.
fn hash_segment(hash: &BlobHash) -> String {
    hash.as_str().replace(':', "%3A")
}

impl BlobControlPlane for HttpControlPlane {
    fn status(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<BlobStatusV1, TransferError>> {
        Box::pin(async move {
            let url = Self::url(
                &access,
                &format!(
                    "v1/cloud/workspaces/{workspace}/blobs/{}",
                    hash_segment(&hash)
                ),
            )?;
            self.send(self.client.get(url), &access.bearer).await
        })
    }

    fn allocate(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
        request: AllocateRequestV1,
    ) -> PortFuture<'_, Result<AllocationV1, TransferError>> {
        Box::pin(async move {
            let url = Self::url(
                &access,
                &format!(
                    "v1/cloud/workspaces/{workspace}/blobs/{}/uploads",
                    hash_segment(&hash)
                ),
            )?;
            self.send(self.client.post(url).json(&request), &access.bearer)
                .await
        })
    }

    fn renew(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        upload: CloudId,
    ) -> PortFuture<'_, Result<AllocationV1, TransferError>> {
        Box::pin(async move {
            let url = Self::url(
                &access,
                &format!(
                    "v1/cloud/workspaces/{workspace}/blob-uploads/{}/capability",
                    upload.as_uuid().hyphenated()
                ),
            )?;
            self.send(self.client.post(url), &access.bearer).await
        })
    }

    fn abort(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        upload: CloudId,
    ) -> PortFuture<'_, Result<(), TransferError>> {
        Box::pin(async move {
            let url = Self::url(
                &access,
                &format!(
                    "v1/cloud/workspaces/{workspace}/blob-uploads/{}",
                    upload.as_uuid().hyphenated()
                ),
            )?;
            let _: serde_json::Value = self.send(self.client.delete(url), &access.bearer).await?;
            Ok(())
        })
    }

    fn read_capability(
        &self,
        access: CloudAccessFacts,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<ReadCapabilityV1, TransferError>> {
        Box::pin(async move {
            let url = Self::url(
                &access,
                &format!(
                    "v1/cloud/workspaces/{workspace}/blobs/{}/read-capability",
                    hash_segment(&hash)
                ),
            )?;
            self.send(self.client.post(url), &access.bearer).await
        })
    }
}
