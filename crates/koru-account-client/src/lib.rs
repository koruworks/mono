// SPDX-License-Identifier: MPL-2.0

//! Typed, bounded client for the Koru account v1 HTTP surface.
//!
//! Provider endpoints never enter this API. The only redirect-like input is
//! the closed [`SymbolicReturnTarget`] enum in the shared wire contract.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::VerifyingKey;
pub use koru_account_wire::MAX_RESPONSE_BODY_BYTES;
use koru_account_wire::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::{Host, Url};
use zeroize::Zeroizing;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

mod diagnostics;
mod subscription;
pub use subscription::CloudSubscriptionStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Delete,
    Get,
    Post,
    Put,
    Patch,
}

pub struct HttpRequest {
    pub method: HttpMethod,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path_present", &!self.path_and_query.is_empty())
            .field("header_count", &self.headers.len())
            .field("body_len", &self.body.len())
            .finish()
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait Transport: Send + Sync + 'static {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    InvalidRequest,
    Unavailable,
    Timeout,
    ConnectionFailed,
    BodyReadFailed,
    ResponseTooLarge,
}

/// Production blocking transport. Redirect following is disabled so neither a
/// gateway nor a provider response can forward account credentials elsewhere.
pub struct ReqwestTransport {
    origin: String,
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    pub fn new(origin: &str) -> Result<Self, TransportError> {
        validate_api_origin(origin)?;
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| TransportError::Unavailable)?;
        Ok(Self {
            origin: origin.trim_end_matches('/').to_owned(),
            client,
        })
    }
}

/// IO-free validation using exactly the account transport's HTTP policy.
/// The returned canonical origin is routing metadata, not a vault namespace.
pub fn validate_api_origin(origin: &str) -> Result<Url, TransportError> {
    let canonical = koru_account_wire::regional_bootstrap::canonical_api_origin(origin)
        .map_err(|_| TransportError::InvalidRequest)?;
    let url = Url::parse(&canonical).map_err(|_| TransportError::InvalidRequest)?;
    if url.scheme() == "http" && !is_loopback_host(&url) {
        return Err(TransportError::InvalidRequest);
    }
    Ok(url)
}

/// The one cleartext exception accepted by the public API (ADR-307). Match the
/// parsed host, never a suffix, so `localhost.example.com` remains remote.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if !(request.path_and_query.starts_with("/v1/account")
            || request.path_and_query.starts_with("/v1/oauth"))
            || request.path_and_query.contains('#')
            || request.body.len() > MAX_REQUEST_BODY_BYTES
        {
            return Err(TransportError::InvalidRequest);
        }
        let url = format!("{}{}", self.origin, request.path_and_query);
        let mut builder = match request.method {
            HttpMethod::Delete => self.client.delete(url),
            HttpMethod::Get => self.client.get(url),
            HttpMethod::Post => self.client.post(url),
            HttpMethod::Put => self.client.put(url),
            HttpMethod::Patch => self.client.patch(url),
        };
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let mut response = builder.body(request.body).send().map_err(|error| {
            if error.is_timeout() {
                TransportError::Timeout
            } else if error.is_connect() {
                TransportError::ConnectionFailed
            } else {
                TransportError::Unavailable
            }
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BODY_BYTES as u64)
        {
            return Err(TransportError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        response
            .by_ref()
            .take((MAX_RESPONSE_BODY_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| TransportError::BodyReadFailed)?;
        if body.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(TransportError::ResponseTooLarge);
        }
        Ok(HttpResponse {
            status: response.status().as_u16(),
            body,
        })
    }
}

pub struct BrowserCsrf(Zeroizing<String>);

impl BrowserCsrf {
    pub fn new(value: String) -> Result<Self, ClientError> {
        validate_credential(&value)?;
        Ok(Self(Zeroizing::new(value)))
    }
}

impl fmt::Debug for BrowserCsrf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserCsrf(<redacted>)")
    }
}

pub enum AccountAuthorization<'a> {
    Bearer(&'a AccessToken),
    WebSession {
        session: &'a WebSessionToken,
        csrf: &'a BrowserCsrf,
    },
    /// Uses the CSRF component embedded by the public API in its one
    /// opaque web-session token; callers never need a second secret channel.
    EmbeddedWebSession(&'a WebSessionToken),
}

impl<'a> AccountAuthorization<'a> {
    pub fn from_web_session(session: &'a WebSessionToken) -> Result<Self, ClientError> {
        embedded_csrf(session)?;
        Ok(Self::EmbeddedWebSession(session))
    }
}

impl fmt::Debug for AccountAuthorization<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bearer(_) => "AccountAuthorization::Bearer(<redacted>)",
            Self::WebSession { .. } | Self::EmbeddedWebSession(_) => {
                "AccountAuthorization::WebSession(<redacted>)"
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    InvalidRequest,
    MissingBrowserOrigin,
    Transport(TransportError),
    /// A refusal that carried the public API's own `DeveloperErrorV1`: the
    /// status and the code are both the service's own words.
    Api {
        status: u16,
        code: ErrorCode,
    },
    RateLimited {
        retry_after_millis: u64,
    },
    /// A non-2xx whose body is not that envelope — a gateway's own 404 page,
    /// an HTML 502, an empty body. The status is then the only thing the
    /// answer says, and it is kept rather than discarded: a service that
    /// does not serve a route is not an outage, and flattening its 404 into
    /// [`Self::InvalidResponse`] is what made it look like one. There is no
    /// code to match on, which is why this is not [`Self::Api`].
    ApiWithoutCode {
        status: u16,
    },
    /// A body that could not be read as what the route promises, on an
    /// answer that claimed to succeed.
    InvalidResponse,
    ContractMismatch,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => f.write_str("the account client request is invalid"),
            Self::MissingBrowserOrigin => f.write_str("a configured browser origin is required"),
            Self::Transport(_) => f.write_str("the public API is unavailable"),
            Self::Api { code, .. } => write!(f, "public API error {}", code.as_str()),
            Self::RateLimited { .. } => f.write_str("public API error rate_limited"),
            Self::ApiWithoutCode { status } => {
                write!(f, "public API error without a code (HTTP {status})")
            }
            Self::InvalidResponse => f.write_str("the public API returned an invalid response"),
            Self::ContractMismatch => f.write_str("the account route contract is inconsistent"),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Clone)]
pub struct AccountClient {
    transport: Arc<dyn Transport>,
    browser_origin: Option<String>,
    diagnostics: Arc<diagnostics::Diagnostics>,
    regional_credentials: Option<koru_account_session::RegionalCredentialPolicy>,
}

impl AccountClient {
    pub fn new(origin: &str) -> Result<Self, ClientError> {
        Ok(Self {
            transport: Arc::new(ReqwestTransport::new(origin).map_err(ClientError::Transport)?),
            browser_origin: None,
            diagnostics: Arc::default(),
            regional_credentials: None,
        })
    }

    pub fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            browser_origin: None,
            diagnostics: Arc::default(),
            regional_credentials: None,
        }
    }

    pub fn with_browser_origin(mut self, origin: String) -> Result<Self, ClientError> {
        validate_origin(&origin)?;
        self.browser_origin = Some(origin.trim_end_matches('/').to_owned());
        Ok(self)
    }

    /// Trusted application configuration shared by both enrollment directions.
    pub fn with_regional_credentials(
        mut self,
        policy: koru_account_session::RegionalCredentialPolicy,
    ) -> Result<Self, ClientError> {
        policy.validate().map_err(|_| ClientError::InvalidRequest)?;
        self.regional_credentials = Some(policy);
        Ok(self)
    }

    pub fn google_web_start(
        &self,
        request: &GoogleWebStartRequestV1,
    ) -> Result<AuthStartResponseV1, ClientError> {
        self.send_json(RouteId::GoogleWebStart, request, None, true)
    }

    pub fn web_exchange(
        &self,
        request: &WebExchangeRequestV1,
    ) -> Result<WebExchangeResponseV1, ClientError> {
        self.send_json(RouteId::WebExchange, request, None, true)
    }

    pub fn google_native_start(
        &self,
        request: &GoogleNativeStartRequestV1,
    ) -> Result<AuthStartResponseV1, ClientError> {
        self.send_json(RouteId::GoogleNativeStart, request, None, false)
    }

    /// Sign in with Apple through the system browser: the same request and
    /// response as Google's native start, sent to Apple's route.
    pub fn apple_native_start(
        &self,
        request: &GoogleNativeStartRequestV1,
    ) -> Result<AuthStartResponseV1, ClientError> {
        self.send_json(RouteId::AppleNativeStart, request, None, false)
    }

    /// Open a flow bound to this device. No URL comes back: V2 moved the
    /// journey into the app, and the four calls below are the whole of it.
    pub fn email_native_start(
        &self,
        request: &EmailNativeStartRequestV1,
    ) -> Result<EmailNativeStartResponseV1, ClientError> {
        self.send_json(RouteId::EmailNativeStart, request, None, false)
    }

    pub fn email_start(
        &self,
        request: &EmailStartRequestV1,
    ) -> Result<EmailStartResponseV1, ClientError> {
        self.send_json(RouteId::EmailStart, request, None, false)
    }

    pub fn email_verify(
        &self,
        request: &EmailVerifyRequestV1,
    ) -> Result<EmailVerifyResponseV1, ClientError> {
        self.send_json(RouteId::EmailVerify, request, None, false)
    }

    pub fn email_complete(
        &self,
        request: &EmailCompleteRequestV1,
    ) -> Result<EmailCompleteResponseV1, ClientError> {
        self.send_json(RouteId::EmailComplete, request, None, false)
    }

    pub fn email_signin(
        &self,
        request: &EmailSignInRequestV1,
    ) -> Result<EmailSignInResponseV1, ClientError> {
        self.send_json(RouteId::EmailSignIn, request, None, false)
    }

    pub fn email_resend(
        &self,
        request: &EmailResendRequestV1,
    ) -> Result<EmailResendResponseV1, ClientError> {
        self.send_json(RouteId::EmailResend, request, None, false)
    }

    pub fn google_native_token_start(
        &self,
        request: &GoogleNativeTokenStartRequestV1,
    ) -> Result<GoogleNativeTokenStartResponseV1, ClientError> {
        self.send_json(RouteId::GoogleNativeTokenStart, request, None, false)
    }

    pub fn google_native_token_complete(
        &self,
        request: &GoogleNativeTokenCompleteRequestV1,
    ) -> Result<GoogleNativeTokenCompleteResponseV1, ClientError> {
        self.send_json(RouteId::GoogleNativeTokenComplete, request, None, false)
    }

    pub fn native_exchange(
        &self,
        request: &NativeExchangeRequestV1,
    ) -> Result<NativeExchangeResponseV1, ClientError> {
        self.send_json(RouteId::NativeExchange, request, None, false)
    }

    pub fn confirm_account_creation(
        &self,
        request: &AccountCreationRequestV1,
    ) -> Result<AccountCreationResponseV1, ClientError> {
        self.send_json(RouteId::ConfirmAccountCreation, request, None, false)
    }

    pub fn cancel_account_creation(
        &self,
        request: &AccountCreationRequestV1,
    ) -> Result<AccountCreationResponseV1, ClientError> {
        self.send_json(RouteId::CancelAccountCreation, request, None, false)
    }

    pub fn native_device_challenge(
        &self,
        request: &NativeExchangeRequestV1,
    ) -> Result<DeviceOwnershipChallengeResponseV1, ClientError> {
        self.send_json(RouteId::NativeDeviceChallenge, request, None, false)
    }

    pub fn device_registration_challenge(
        &self,
        request: &RegisterDeviceRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<DeviceOwnershipChallengeResponseV1, ClientError> {
        self.send_json(
            RouteId::DeviceRegistrationChallenge,
            request,
            Some(authorization),
            false,
        )
    }

    pub fn refresh(&self, request: &RefreshRequestV1) -> Result<RefreshResponseV1, ClientError> {
        self.send_json(RouteId::Refresh, request, None, false)
    }

    /// Observe current native credential ownership without rotating it. This
    /// observation is not an access token or continuing authorization.
    pub fn inspect_native_session(
        &self,
        request: &NativeSessionInspectRequestV1,
    ) -> Result<SessionSummaryV1, ClientError> {
        self.send_json(RouteId::InspectNativeSession, request, None, false)
    }

    pub fn logout(
        &self,
        request: &LogoutRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<EmptyResponseV1, ClientError> {
        self.send_json(RouteId::Logout, request, Some(authorization), false)
    }

    pub fn session(
        &self,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<SessionSummaryV1, ClientError> {
        self.send_empty(RouteId::Session, Some(authorization))
    }

    pub fn account(
        &self,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<AccountSummaryV1, ClientError> {
        self.send_empty(RouteId::Account, Some(authorization))
    }

    pub fn update_account(
        &self,
        request: &UpdateAccountRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<AccountSummaryV1, ClientError> {
        self.send_json(RouteId::UpdateAccount, request, Some(authorization), false)
    }

    pub fn jwks(&self) -> Result<JwksResponseV1, ClientError> {
        self.send_empty(RouteId::Jwks, None)
    }

    /// Enrollment-worker-only verification seam. The sole refresh path is
    /// this client's 10-second, response-bounded JWKS request.
    pub fn verify_device_attestation(
        &self,
        attestation: &koru_account_session::SignedDeviceAttestationV1,
        issuer: &str,
        now_millis: i64,
        cached: &mut JwksResponseV1,
    ) -> Result<koru_account_session::VerifiedDeviceAttestationV1, AttestationVerificationError>
    {
        verify_device_attestation_with_policy_and_refresh(
            attestation,
            issuer,
            now_millis,
            cached,
            self.regional_credentials.as_ref(),
            || self.jwks(),
        )
    }

    pub fn link_google_start(
        &self,
        request: &LinkGoogleStartRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<AuthStartResponseV1, ClientError> {
        self.send_json(
            RouteId::LinkGoogleStart,
            request,
            Some(authorization),
            false,
        )
    }

    pub fn link_google_continue(
        &self,
        request: &LinkGoogleContinueRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<AddGoogleCandidateV1, ClientError> {
        self.send_json(
            RouteId::LinkGoogleContinue,
            request,
            Some(authorization),
            false,
        )
    }

    pub fn confirm_add_google(
        &self,
        request: &ConfirmAddGoogleRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ConfirmAddGoogleResponseV1, ClientError> {
        self.send_json(
            RouteId::ConfirmAddGoogle,
            request,
            Some(authorization),
            false,
        )
    }

    pub fn cancel_add_google(
        &self,
        request: &CancelAddGoogleRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CancelAddGoogleResponseV1, ClientError> {
        self.send_json(
            RouteId::CancelAddGoogle,
            request,
            Some(authorization),
            false,
        )
    }

    pub fn profiles(
        &self,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileListResponseV1, ClientError> {
        self.send_empty(RouteId::ListProfiles, Some(authorization))
    }

    pub fn create_profile(
        &self,
        request: &CreateProfileRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileSummaryV1, ClientError> {
        self.send_json(RouteId::CreateProfile, request, Some(authorization), false)
    }

    pub fn attach_profile(
        &self,
        request: &AttachProfileRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileSummaryV1, ClientError> {
        self.send_json(RouteId::AttachProfile, request, Some(authorization), false)
    }

    pub fn rebind_profile(
        &self,
        request: &RebindProfileRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileSummaryV1, ClientError> {
        self.send_json(RouteId::RebindProfile, request, Some(authorization), false)
    }

    pub fn remove_profile_authorization(
        &self,
        profile_id: koru_account_core::ProfileId,
        request: &RemoveProfileAuthorizationRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileAuthorizationRemovalResponseV1, ClientError> {
        self.send_json_at(
            RouteId::RemoveProfileAuthorization,
            render_route_path(RouteId::RemoveProfileAuthorization, profile_id, None)?,
            request,
            authorization,
        )
    }
    pub fn delete_profile(
        &self,
        profile_id: koru_account_core::ProfileId,
        request: &DeleteProfileRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileLifecycleResponseV1, ClientError> {
        self.send_json_at(
            RouteId::DeleteProfile,
            render_route_path(RouteId::DeleteProfile, profile_id, None)?,
            request,
            authorization,
        )
    }

    pub fn restore_profile(
        &self,
        profile_id: koru_account_core::ProfileId,
        request: &RestoreProfileRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProfileLifecycleResponseV1, ClientError> {
        self.send_json_at(
            RouteId::RestoreProfile,
            render_route_path(RouteId::RestoreProfile, profile_id, None)?,
            request,
            authorization,
        )
    }

    pub fn resolve_profile_lifecycles(
        &self,
        request: &ResolveProfileLifecyclesRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ResolveProfileLifecyclesResponseV1, ClientError> {
        self.send_json(
            RouteId::ResolveProfileLifecycles,
            request,
            Some(authorization),
            false,
        )
    }
    pub fn register_device(
        &self,
        request: &RegisterDeviceRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<DeviceSummaryV1, ClientError> {
        self.send_json(RouteId::RegisterDevice, request, Some(authorization), false)
    }

    pub fn devices(
        &self,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<DeviceListResponseV1, ClientError> {
        self.send_empty(RouteId::ListDevices, Some(authorization))
    }

    pub fn enrollment_candidates(
        &self,
        profile_id: koru_account_core::ProfileId,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<EnrollmentCandidatesResponseV1, ClientError> {
        self.send_empty_at(
            RouteId::ListEnrollmentCandidates,
            render_route_path(RouteId::ListEnrollmentCandidates, profile_id, None)?,
            authorization,
        )
    }

    /// One bounded page of Cloud eligibility pairs (TECHNICAL section 6).
    /// Pass the previous page's `next` to continue; the cursor is a position,
    /// never authority, and a page is not an account-wide snapshot.
    pub fn cloud_workspaces(
        &self,
        after: Option<&CloudWorkspaceCursorV1>,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudWorkspacePageV1, ClientError> {
        let route = route_by_id(RouteId::ListCloudWorkspaces);
        let path = match after {
            None => route.path.to_owned(),
            Some(cursor) => format!(
                "{}?after_profile_id={}&after_workspace_id={}",
                route.path,
                cursor.profile_id.to_storage(),
                cursor.workspace_id.to_storage()
            ),
        };
        self.send_empty_at(RouteId::ListCloudWorkspaces, path, authorization)
    }

    /// Select one exact Profile/Workspace pair for Cloud sync on this device
    /// (TECHNICAL section 7). Repeating a selected pair answers `selected`.
    pub fn select_profile_cloud(
        &self,
        profile_id: koru_account_core::ProfileId,
        request: &CloudProfileSelectionRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudProfileSelectionResponseV1, ClientError> {
        self.send_json_at(
            RouteId::SelectProfileCloud,
            render_route_path(RouteId::SelectProfileCloud, profile_id, None)?,
            request,
            authorization,
        )
    }

    /// Identity-only bootstrap facts for one authorized Workspace, including
    /// the manifest's retained Store document id.
    /// One call per Workspace; `Api { status: 503 }` means the hosted mapping
    /// does not exist yet, never a reason to invent one.
    pub fn cloud_workspace_descriptor(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        profile_id: koru_account_core::ProfileId,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudWorkspaceDescriptorV1, ClientError> {
        let path = render_workspace_route_path(RouteId::DescribeCloudWorkspace, workspace_id)?;
        self.send_empty_at(
            RouteId::DescribeCloudWorkspace,
            format!("{path}?profile_id={}", profile_id.to_storage()),
            authorization,
        )
    }

    /// One Workspace's current Cloud lifecycle row in whatever state it is,
    /// with the authorization version a mutation must quote. `already_absent`
    /// with version 0 means no hosted row exists yet.
    pub fn inspect_workspace_cloud(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<WorkspaceCloudLifecycleResponseV1, ClientError> {
        let route = route_by_id(RouteId::InspectWorkspaceCloud);
        let path = route
            .path
            .replace("{workspace_id}", &workspace_id.to_storage());
        if path.contains(['{', '}'])
            || route_for(route.method, &path).map(|value| value.id) != Some(route.id)
        {
            return Err(ClientError::ContractMismatch);
        }
        self.send_empty_at(RouteId::InspectWorkspaceCloud, path, authorization)
    }

    /// Ask the hosted account for the bytes this device must sign to turn
    /// Cloud sync on for one Workspace (workspace-cloud-lifecycle-v1 §2).
    /// `expected_authorization_version` is the version this device believes
    /// the hosted row holds — `0` when it believes there is no row yet — and a
    /// wrong guess is refused with `workspace_version_mismatch` rather than
    /// applied. Repeating an `operation_id` with identical parameters answers
    /// the same challenge; a challenge that expired needs a fresh one.
    pub fn begin_workspace_cloud_activation(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        request: &BeginWorkspaceCloudActivationRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<WorkspaceCloudActivationChallengeV1, ClientError> {
        self.send_json_at(
            RouteId::BeginWorkspaceCloudActivation,
            render_workspace_route_path(RouteId::BeginWorkspaceCloudActivation, workspace_id)?,
            request,
            authorization,
        )
    }

    /// Present the device-key signature over the challenge's `signing_bytes`
    /// and commit the activation. The response carries the authorization
    /// version and incarnation every later mutation of this Workspace must
    /// quote, so a caller that discards it has to rediscover both.
    pub fn complete_workspace_cloud_activation(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        request: &CompleteWorkspaceCloudActivationRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<WorkspaceCloudLifecycleResponseV1, ClientError> {
        self.send_json_at(
            RouteId::CompleteWorkspaceCloudActivation,
            render_workspace_route_path(RouteId::CompleteWorkspaceCloudActivation, workspace_id)?,
            request,
            authorization,
        )
    }

    /// Turn Cloud sync off for one Workspace while the hosted copy is kept.
    /// Both the current authorization version and the current incarnation are
    /// quoted, so this cannot disable a Workspace that was re-created or
    /// changed underneath the caller.
    pub fn disable_workspace_cloud(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        request: &MutateWorkspaceCloudRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<WorkspaceCloudLifecycleResponseV1, ClientError> {
        self.send_json_at(
            RouteId::DisableWorkspaceCloud,
            render_workspace_route_path(RouteId::DisableWorkspaceCloud, workspace_id)?,
            request,
            authorization,
        )
    }

    /// One short-lived Cloud sync credential for an exact Workspace under the
    /// requested membership Profile, with the worker routes for the Workspace
    /// and Profile datasets. The response is evidence to verify and correlate,
    /// never authority by itself.
    pub fn issue_cloud_sync_token(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        request: &SyncTokenRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<SyncTokenResponseV1, ClientError> {
        self.send_json_at(
            RouteId::IssueCloudSyncToken,
            render_workspace_route_path(RouteId::IssueCloudSyncToken, workspace_id)?,
            request,
            authorization,
        )
    }

    // ----- The v2 Cloud catalog (root-catalog Plan 3.1's contract, called by
    // Plan 3.2's coordinator). Every route is native-device-authenticated and
    // bounded by the shared body caps; none carries a name, a credential or a
    // hidden subject.

    /// Get or create the account's one root-catalog binding, declaring what
    /// this device already holds. Idempotent; a device of another account is
    /// refused with `catalog_binding_conflict` and nothing is written. A 404
    /// is a service that does not answer v2 yet, which the caller reads as
    /// the v1 compatibility window.
    pub fn bind_cloud_catalog(
        &self,
        request: &CloudCatalogBindingRequestV2,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudCatalogBindingV2, ClientError> {
        self.send_json(
            RouteId::BindAccountCatalog,
            request,
            Some(authorization),
            false,
        )
    }

    /// The account's admitted catalog units with their frontiers and
    /// projection watermarks. `after` continues a paged answer from the last
    /// unit listed; the cursor is a position, never authority.
    pub fn cloud_root_catalog(
        &self,
        after: Option<&CloudUnitRefV2>,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudRootCatalogV2, ClientError> {
        let route = route_by_id(RouteId::ReadCloudRootCatalog);
        let path = match after {
            None => route.path.to_owned(),
            Some(unit) => {
                let (kind, subject) = unit_cursor(*unit);
                format!(
                    "{}?after_kind={kind}&after_subject_id={subject}",
                    route.path
                )
            }
        };
        self.send_empty_at(RouteId::ReadCloudRootCatalog, path, authorization)
    }

    /// One unit from the device's accepted frontier, through the export
    /// boundary; an empty frontier asks for a snapshot.
    pub fn pull_cloud_root_unit(
        &self,
        request: &CloudRootUnitRequestV2,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudRootUnitUpdatesV2, ClientError> {
        self.send_json(
            RouteId::PullCloudRootUnit,
            request,
            Some(authorization),
            false,
        )
    }

    /// Admit one unit's derived bytes and read the join receipt.
    pub fn push_cloud_root_unit(
        &self,
        request: &CloudRootUnitUpdatesV2,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudRootJoinReceiptV2, ClientError> {
        self.send_json(
            RouteId::PushCloudRootUnit,
            request,
            Some(authorization),
            false,
        )
    }

    /// One short-lived credential for one Cloud scope. The response is
    /// evidence to validate and correlate, never authority by itself.
    pub fn issue_cloud_sync_token_v2(
        &self,
        request: &SyncTokenRequestV2,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<SyncTokenResponseV2, ClientError> {
        let response: SyncTokenResponseV2 = self.send_json(
            RouteId::IssueCloudSyncTokenV2,
            request,
            Some(authorization),
            false,
        )?;
        response
            .validate()
            .map_err(|_| ClientError::InvalidResponse)?;
        if response.scope != request.scope {
            return Err(ClientError::InvalidResponse);
        }
        Ok(response)
    }

    /// Identity-only bootstrap facts for one authorized Workspace with its
    /// placement as the committed projection holds it. No Profile is named
    /// in the request: the former Profile is not a prerequisite.
    pub fn cloud_workspace_descriptor_v2(
        &self,
        workspace_id: koru_account_core::WorkspaceId,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudWorkspaceDescriptorV2, ClientError> {
        self.send_empty_at(
            RouteId::DescribeCloudWorkspaceV2,
            render_workspace_route_path(RouteId::DescribeCloudWorkspaceV2, workspace_id)?,
            authorization,
        )
    }

    /// One discovery receipt from the committed projection: scope,
    /// authorization generation, coverage and results grouped by current
    /// placement. `after` continues a paged answer.
    pub fn cloud_discovery(
        &self,
        scope: CloudDiscoveryScopeV2,
        after: Option<&CloudDiscoveryCursorV2>,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudDiscoveryReceiptV2, ClientError> {
        let route = route_by_id(RouteId::DiscoverCloudCatalog);
        let scope = match scope {
            CloudDiscoveryScopeV2::UserCatalog => "user_catalog",
            CloudDiscoveryScopeV2::Workspaces => "workspaces",
        };
        let path = match after {
            None => format!("{}?scope={scope}", route.path),
            Some(cursor) => format!(
                "{}?scope={scope}&after_workspace_id={}",
                route.path,
                cursor.workspace_id.to_storage()
            ),
        };
        self.send_empty_at(RouteId::DiscoverCloudCatalog, path, authorization)
    }

    pub fn issue_device_attestation(
        &self,
        profile_id: koru_account_core::ProfileId,
        request: &DeviceAttestationRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<DeviceAttestationResponseV1, ClientError> {
        self.send_json_at(
            RouteId::IssueDeviceAttestation,
            render_route_path(RouteId::IssueDeviceAttestation, profile_id, None)?,
            request,
            authorization,
        )
    }

    pub fn provision_device(
        &self,
        profile_id: koru_account_core::ProfileId,
        device_id: koru_account_core::DeviceId,
        request: &ProvisionDeviceRequestV1,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<ProvisionDeviceResponseV1, ClientError> {
        self.send_json_at(
            RouteId::ProvisionDevice,
            render_route_path(RouteId::ProvisionDevice, profile_id, Some(device_id))?,
            request,
            authorization,
        )
    }

    fn send_empty<Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        authorization: Option<&AccountAuthorization<'_>>,
    ) -> Result<Response, ClientError> {
        self.execute(route_id, Vec::new(), authorization, false, None)
    }

    fn send_empty_at<Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        path: String,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<Response, ClientError> {
        self.execute(route_id, Vec::new(), Some(authorization), false, Some(path))
    }

    fn send_json<Request: Serialize, Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        request: &Request,
        authorization: Option<&AccountAuthorization<'_>>,
        browser_public: bool,
    ) -> Result<Response, ClientError> {
        let body = serde_json::to_vec(request).map_err(|_| ClientError::InvalidRequest)?;
        if body.len() > MAX_REQUEST_BODY_BYTES {
            return Err(ClientError::InvalidRequest);
        }
        self.execute(route_id, body, authorization, browser_public, None)
    }

    fn send_json_at<Request: Serialize, Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        path: String,
        request: &Request,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<Response, ClientError> {
        let body = serde_json::to_vec(request).map_err(|_| ClientError::InvalidRequest)?;
        if body.len() > MAX_REQUEST_BODY_BYTES {
            return Err(ClientError::InvalidRequest);
        }
        self.execute(route_id, body, Some(authorization), false, Some(path))
    }

    fn execute<Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        body: Vec<u8>,
        authorization: Option<&AccountAuthorization<'_>>,
        browser_public: bool,
        path: Option<String>,
    ) -> Result<Response, ClientError> {
        let started = std::time::Instant::now();
        let mut status = None;
        let result = self.execute_inner(
            route_id,
            body,
            authorization,
            browser_public,
            path,
            &mut status,
        );
        self.diagnostics
            .observe(route_id, status, result.as_ref().err(), started.elapsed());
        result
    }

    fn execute_inner<Response: DeserializeOwned>(
        &self,
        route_id: RouteId,
        body: Vec<u8>,
        authorization: Option<&AccountAuthorization<'_>>,
        browser_public: bool,
        path: Option<String>,
        status: &mut Option<u16>,
    ) -> Result<Response, ClientError> {
        let route = route_by_id(route_id);
        if (authorization.is_some() && route.access == Access::Public)
            || (authorization.is_none() && route.access != Access::Public)
        {
            return Err(ClientError::ContractMismatch);
        }
        let mut headers = Vec::new();
        if route.request_location == RequestLocation::Body {
            headers.push(("content-type".into(), "application/json".into()));
        }
        if browser_public {
            let origin = self
                .browser_origin
                .as_ref()
                .ok_or(ClientError::MissingBrowserOrigin)?;
            headers.push(("origin".into(), origin.clone()));
        }
        if let Some(authorization) = authorization {
            match authorization {
                AccountAuthorization::Bearer(token) => {
                    validate_credential(&token.0)?;
                    headers.push(("authorization".into(), format!("Bearer {}", token.0)));
                }
                AccountAuthorization::WebSession { session, csrf } => {
                    validate_credential(&session.0)?;
                    let origin = self
                        .browser_origin
                        .as_ref()
                        .ok_or(ClientError::MissingBrowserOrigin)?;
                    headers.push((
                        "cookie".into(),
                        format!(
                            "{WEB_SESSION_COOKIE}={}; {CSRF_COOKIE}={}",
                            session.0,
                            csrf.0.as_str()
                        ),
                    ));
                    if matches!(
                        route.method,
                        Method::Delete | Method::Post | Method::Put | Method::Patch
                    ) {
                        headers.push((CSRF_HEADER.into(), csrf.0.to_string()));
                        headers.push(("origin".into(), origin.clone()));
                    }
                }
                AccountAuthorization::EmbeddedWebSession(session) => {
                    validate_credential(&session.0)?;
                    let csrf = embedded_csrf(session)?;
                    let origin = self
                        .browser_origin
                        .as_ref()
                        .ok_or(ClientError::MissingBrowserOrigin)?;
                    headers.push((
                        "cookie".into(),
                        format!("{WEB_SESSION_COOKIE}={}; {CSRF_COOKIE}={csrf}", session.0),
                    ));
                    if matches!(
                        route.method,
                        Method::Delete | Method::Post | Method::Put | Method::Patch
                    ) {
                        headers.push((CSRF_HEADER.into(), csrf.to_owned()));
                        headers.push(("origin".into(), origin.clone()));
                    }
                }
            }
        }
        let path_and_query = path.unwrap_or_else(|| route.path.to_owned());
        let response = self
            .transport
            .send(HttpRequest {
                method: match route.method {
                    Method::Delete => HttpMethod::Delete,
                    Method::Get => HttpMethod::Get,
                    Method::Post => HttpMethod::Post,
                    Method::Put => HttpMethod::Put,
                    Method::Patch => HttpMethod::Patch,
                },
                path_and_query,
                headers,
                body,
            })
            .map_err(ClientError::Transport)?;
        *status = Some(response.status);
        if response.body.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(ClientError::Transport(TransportError::ResponseTooLarge));
        }
        if !(200..300).contains(&response.status) {
            // A refusal is read from the envelope, but the status is the
            // service's answer whether or not the envelope is there: a
            // gateway, a proxy or a route the deployment does not serve
            // answers in its own words, and the status is then all there is
            // to go on. Keep it. Discarding it is how a 404 on a route the
            // service does not serve yet became indistinguishable from an
            // outage.
            let Ok(error) = serde_json::from_slice::<DeveloperErrorV1>(&response.body) else {
                return Err(ClientError::ApiWithoutCode {
                    status: response.status,
                });
            };
            if response.status == 429 && error.code.as_str() == ErrorCode::RATE_LIMITED {
                return Err(ClientError::RateLimited {
                    retry_after_millis: error
                        .retry_after_millis
                        .unwrap_or(60_000)
                        .clamp(1_000, 600_000),
                });
            }
            return Err(ClientError::Api {
                status: response.status,
                code: error.code,
            });
        }
        serde_json::from_slice(&response.body).map_err(|_| ClientError::InvalidResponse)
    }
}

fn render_route_path(
    route_id: RouteId,
    profile_id: koru_account_core::ProfileId,
    device_id: Option<koru_account_core::DeviceId>,
) -> Result<String, ClientError> {
    let route = route_by_id(route_id);
    let profile_placeholders = route.path.matches("{profile_id}").count();
    let device_placeholders = route.path.matches("{device_id}").count();
    if profile_placeholders != 1 || device_placeholders != usize::from(device_id.is_some()) {
        return Err(ClientError::ContractMismatch);
    }
    let mut path = route.path.replace("{profile_id}", &profile_id.to_storage());
    match (path.contains("{device_id}"), device_id) {
        (true, Some(device_id)) => {
            path = path.replace("{device_id}", &device_id.to_storage());
        }
        (false, None) => {}
        _ => return Err(ClientError::ContractMismatch),
    }
    if path.len() > 256
        || path.contains(['{', '}'])
        || route_for(route.method, &path).map(|value| value.id) != Some(route_id)
    {
        return Err(ClientError::ContractMismatch);
    }
    Ok(path)
}

/// The workspace-scoped sibling of [`render_route_path`]. Every Workspace
/// route carries exactly one `{workspace_id}` and nothing else, and the
/// rendered path is read back through the route table so a catalog edit that
/// changed a path shape fails here rather than posting a device signature at
/// whatever route happens to match.
/// A catalog unit as the `after_*` query values of the catalog listing: the
/// wire tag of its kind and its subject's canonical id.
fn unit_cursor(unit: CloudUnitRefV2) -> (&'static str, String) {
    match unit {
        CloudUnitRefV2::ProfileCatalog { subject_id } => {
            ("profile_catalog", subject_id.to_storage())
        }
        CloudUnitRefV2::WorkspaceCatalog { subject_id } => {
            ("workspace_catalog", subject_id.to_storage())
        }
        CloudUnitRefV2::PersonalV2 { subject_id } => ("personal_v2", subject_id.to_storage()),
    }
}

fn render_workspace_route_path(
    route_id: RouteId,
    workspace_id: koru_account_core::WorkspaceId,
) -> Result<String, ClientError> {
    let route = route_by_id(route_id);
    if route.path.matches("{workspace_id}").count() != 1 {
        return Err(ClientError::ContractMismatch);
    }
    let path = route
        .path
        .replace("{workspace_id}", &workspace_id.to_storage());
    if path.len() > 256
        || path.contains(['{', '}'])
        || route_for(route.method, &path).map(|value| value.id) != Some(route_id)
    {
        return Err(ClientError::ContractMismatch);
    }
    Ok(path)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestationVerificationError {
    InvalidKeySet,
    InvalidAttestation,
    RefreshFailed,
}

/// Verify with the cached, purpose-specific attestation set and perform at
/// most one refresh only for an unknown `kid`. Callers run this blocking seam
/// on the enrollment worker, never an Agent answering thread.
fn verify_device_attestation_with_policy_and_refresh<F>(
    attestation: &koru_account_session::SignedDeviceAttestationV1,
    issuer: &str,
    now_millis: i64,
    cached: &mut JwksResponseV1,
    policy: Option<&koru_account_session::RegionalCredentialPolicy>,
    refresh: F,
) -> Result<koru_account_session::VerifiedDeviceAttestationV1, AttestationVerificationError>
where
    F: FnOnce() -> Result<JwksResponseV1, ClientError>,
{
    let keys = attestation_keys(cached)?;
    match koru_account_session::verify_device_attestation_with_policy(
        attestation,
        &keys,
        issuer,
        now_millis,
        policy,
    ) {
        Ok(verified) => Ok(verified),
        Err(koru_account_session::JwtVerificationError::UnknownKey) => {
            let fresh = refresh().map_err(|_| AttestationVerificationError::RefreshFailed)?;
            let keys = attestation_keys(&fresh)?;
            let verified = koru_account_session::verify_device_attestation_with_policy(
                attestation,
                &keys,
                issuer,
                now_millis,
                policy,
            )
            .map_err(|_| AttestationVerificationError::InvalidAttestation)?;
            *cached = fresh;
            Ok(verified)
        }
        Err(_) => Err(AttestationVerificationError::InvalidAttestation),
    }
}

fn attestation_keys(
    jwks: &JwksResponseV1,
) -> Result<BTreeMap<String, VerifyingKey>, AttestationVerificationError> {
    if jwks.attestation_keys.is_empty() || jwks.attestation_keys.len() > 16 {
        return Err(AttestationVerificationError::InvalidKeySet);
    }
    let mut result = BTreeMap::new();
    for key in &jwks.attestation_keys {
        if key.kty != "OKP"
            || key.crv != "Ed25519"
            || key.key_use != "sig"
            || key.alg != "EdDSA"
            || key.kid.is_empty()
            || key.kid.len() > 128
            || !key
                .kid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || key.x.len() != 43
        {
            return Err(AttestationVerificationError::InvalidKeySet);
        }
        let bytes: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&key.x)
            .map_err(|_| AttestationVerificationError::InvalidKeySet)?
            .try_into()
            .map_err(|_| AttestationVerificationError::InvalidKeySet)?;
        if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes) != key.x {
            return Err(AttestationVerificationError::InvalidKeySet);
        }
        let verifying = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| AttestationVerificationError::InvalidKeySet)?;
        if result.insert(key.kid.clone(), verifying).is_some() {
            return Err(AttestationVerificationError::InvalidKeySet);
        }
    }
    Ok(result)
}

fn embedded_csrf(session: &WebSessionToken) -> Result<&str, ClientError> {
    let mut parts = session.0.split('.');
    let id = parts.next().ok_or(ClientError::InvalidRequest)?;
    let secret = parts.next().ok_or(ClientError::InvalidRequest)?;
    let csrf = parts.next().ok_or(ClientError::InvalidRequest)?;
    if parts.next().is_some() || id.is_empty() || secret.is_empty() || csrf.is_empty() {
        return Err(ClientError::InvalidRequest);
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(csrf)
        .map_err(|_| ClientError::InvalidRequest)?;
    if decoded.len() != 32 {
        return Err(ClientError::InvalidRequest);
    }
    Ok(csrf)
}

fn validate_credential(value: &str) -> Result<(), ClientError> {
    if value.is_empty()
        || value.len() > 4096
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b';' && byte != b',')
    {
        return Err(ClientError::InvalidRequest);
    }
    Ok(())
}

fn validate_origin(value: &str) -> Result<(), ClientError> {
    let url = Url::parse(value).map_err(|_| ClientError::InvalidRequest)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
