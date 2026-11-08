// SPDX-License-Identifier: MPL-2.0

use schemars::{Schema, SchemaGenerator};

use crate::*;

pub type SchemaFn = fn(&mut SchemaGenerator) -> Schema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    Delete,
    Get,
    Post,
    Put,
    Patch,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Public,
    WebSession,
    AccountSession,
    RecentAuthentication,
    NativeRecentAuthentication,
    NativeDeviceSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestLocation {
    None,
    Query,
    Body,
    /// `application/x-www-form-urlencoded` body. Only Apple's `form_post`
    /// callbacks use it; the decoded fields are then treated as a query.
    Form,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum RouteId {
    ConfirmAccountCreation,
    CancelAccountCreation,
    GoogleWebStart,
    GoogleWebCallback,
    WebExchange,
    GoogleNativeStart,
    GoogleNativeCallback,
    GoogleNativeTokenStart,
    GoogleNativeTokenComplete,
    NativeExchange,
    NativeDeviceChallenge,
    DeviceRegistrationChallenge,
    Refresh,
    Logout,
    Session,
    Account,
    UpdateAccount,
    Jwks,
    LinkGoogleStart,
    LinkGoogleContinue,
    ConfirmAddGoogle,
    CancelAddGoogle,
    ListProfiles,
    CreateProfile,
    AttachProfile,
    RebindProfile,
    RemoveProfileAuthorization,
    DeleteProfile,
    RestoreProfile,
    ResolveProfileLifecycles,
    RegisterDevice,
    ListDevices,
    ListEnrollmentCandidates,
    IssueDeviceAttestation,
    ProvisionDevice,
    BeginWorkspaceCloudActivation,
    CompleteWorkspaceCloudActivation,
    DisableWorkspaceCloud,
    DeleteWorkspaceCloud,
    InspectNativeSession,
    EmailStart,
    EmailVerify,
    EmailComplete,
    EmailSignIn,
    EmailResend,
    EmailNativeStart,
    IssueCloudSyncToken,
    ListCloudWorkspaces,
    SelectProfileCloud,
    DescribeCloudWorkspace,
    InspectWorkspaceCloud,
    AppleWebStart,
    AppleWebCallback,
    AppleNativeStart,
    AppleNativeCallback,
    OAuthConsentRead,
    OAuthConsentDecide,
    ListAppConnections,
    UpdateAppConnection,
    RevokeAppConnection,
}

impl RouteId {
    pub const fn operation_id(self) -> &'static str {
        match self {
            Self::GoogleWebStart => "account.google_web_start",
            Self::GoogleWebCallback => "account.google_web_callback",
            Self::WebExchange => "account.web_exchange",
            Self::GoogleNativeStart => "account.google_native_start",
            Self::GoogleNativeCallback => "account.google_native_callback",
            Self::GoogleNativeTokenStart => "account.google_native_token_start",
            Self::GoogleNativeTokenComplete => "account.google_native_token_complete",
            Self::ConfirmAccountCreation => "account.creation_confirm",
            Self::CancelAccountCreation => "account.creation_cancel",
            Self::NativeExchange => "account.native_exchange",
            Self::NativeDeviceChallenge => "account.native_device_challenge",
            Self::DeviceRegistrationChallenge => "account.device_registration_challenge",
            Self::Refresh => "account.refresh",
            Self::InspectNativeSession => "account.native_session_inspect",
            Self::Logout => "account.logout",
            Self::Session => "account.session",
            Self::Account => "account.get",
            Self::UpdateAccount => "account.update",
            Self::Jwks => "account.jwks",
            Self::LinkGoogleStart => "account.link_google_start",
            Self::LinkGoogleContinue => "account.link_google_continue",
            Self::ConfirmAddGoogle => "account.add_google_confirm",
            Self::CancelAddGoogle => "account.add_google_cancel",
            Self::ListProfiles => "account.profiles_list",
            Self::CreateProfile => "account.profiles_create",
            Self::AttachProfile => "account.profiles_attach",
            Self::RebindProfile => "account.profiles_rebind",
            Self::RemoveProfileAuthorization => "account.profile_authorization_remove",
            Self::DeleteProfile => "account.profile_delete",
            Self::RestoreProfile => "account.profile_restore",
            Self::ResolveProfileLifecycles => "account.profile_lifecycle_resolve",
            Self::RegisterDevice => "account.devices_register",
            Self::ListDevices => "account.devices_list",
            Self::ListEnrollmentCandidates => "account.enrollment_candidates_list",
            Self::IssueDeviceAttestation => "account.device_attestations_issue",
            Self::ProvisionDevice => "account.device_provisioning_update",
            Self::EmailStart => "account.email_start",
            Self::EmailVerify => "account.email_verify",
            Self::EmailComplete => "account.email_complete",
            Self::EmailSignIn => "account.email_signin",
            Self::EmailResend => "account.email_resend",
            Self::EmailNativeStart => "account.email_native_start",
            Self::BeginWorkspaceCloudActivation => "account.workspace_cloud_activation_begin",
            Self::CompleteWorkspaceCloudActivation => "account.workspace_cloud_activation_complete",
            Self::DisableWorkspaceCloud => "account.workspace_cloud_disable",
            Self::DeleteWorkspaceCloud => "account.workspace_cloud_delete",
            Self::IssueCloudSyncToken => "account.cloud_sync_token_issue",
            Self::ListCloudWorkspaces => "account.cloud_workspaces_list",
            Self::SelectProfileCloud => "account.profile_cloud_selection_update",
            Self::DescribeCloudWorkspace => "account.workspace_cloud_descriptor_read",
            Self::InspectWorkspaceCloud => "account.workspace_cloud_read",
            Self::AppleWebStart => "account.apple_web_start",
            Self::AppleWebCallback => "account.apple_web_callback",
            Self::AppleNativeStart => "account.apple_native_start",
            Self::AppleNativeCallback => "account.apple_native_callback",
            Self::OAuthConsentRead => "account.oauth_consent_read",
            Self::OAuthConsentDecide => "account.oauth_consent_decide",
            Self::ListAppConnections => "account.app_connections_list",
            Self::UpdateAppConnection => "account.app_connection_update",
            Self::RevokeAppConnection => "account.app_connection_revoke",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub id: RouteId,
    pub method: Method,
    pub path: &'static str,
    pub access: Access,
    pub request: SchemaFn,
    pub response: SchemaFn,
    pub error: SchemaFn,
    pub request_location: RequestLocation,
    pub summary: &'static str,
}

macro_rules! schema_fn {
    ($name:ident, $type:ty) => {
        fn $name(generator: &mut SchemaGenerator) -> Schema {
            generator.subschema_for::<$type>()
        }
    };
}

schema_fn!(empty_request, EmptyRequestV1);
schema_fn!(creation_request, AccountCreationRequestV1);
schema_fn!(creation_response, AccountCreationResponseV1);
schema_fn!(
    native_session_inspect_request,
    NativeSessionInspectRequestV1
);
schema_fn!(empty_response, EmptyResponseV1);
schema_fn!(developer_error, DeveloperErrorV1);
schema_fn!(web_start_request, GoogleWebStartRequestV1);
schema_fn!(auth_start_response, AuthStartResponseV1);
schema_fn!(callback_request, GoogleCallbackRequestV1);
schema_fn!(apple_callback_request, AppleCallbackRequestV1);
schema_fn!(web_exchange_request, WebExchangeRequestV1);
schema_fn!(web_exchange_response, WebExchangeResponseV1);
schema_fn!(native_start_request, GoogleNativeStartRequestV1);
schema_fn!(native_token_start_request, GoogleNativeTokenStartRequestV1);
schema_fn!(
    native_token_start_response,
    GoogleNativeTokenStartResponseV1
);
schema_fn!(
    native_token_complete_request,
    GoogleNativeTokenCompleteRequestV1
);
schema_fn!(
    native_token_complete_response,
    GoogleNativeTokenCompleteResponseV1
);
schema_fn!(native_exchange_request, NativeExchangeRequestV1);
schema_fn!(native_exchange_response, NativeExchangeResponseV1);
schema_fn!(
    device_ownership_challenge_response,
    DeviceOwnershipChallengeResponseV1
);
schema_fn!(refresh_request, RefreshRequestV1);
schema_fn!(refresh_response, RefreshResponseV1);
schema_fn!(logout_request, LogoutRequestV1);
schema_fn!(session_response, SessionSummaryV1);
schema_fn!(account_response, AccountSummaryV1);
schema_fn!(update_account_request, UpdateAccountRequestV1);
schema_fn!(jwks_response, JwksResponseV1);
schema_fn!(link_start_request, LinkGoogleStartRequestV1);
schema_fn!(link_continue_request, LinkGoogleContinueRequestV1);
schema_fn!(add_google_candidate, AddGoogleCandidateV1);
schema_fn!(confirm_add_google_request, ConfirmAddGoogleRequestV1);
schema_fn!(confirm_add_google_response, ConfirmAddGoogleResponseV1);
schema_fn!(cancel_add_google_request, CancelAddGoogleRequestV1);
schema_fn!(cancel_add_google_response, CancelAddGoogleResponseV1);
schema_fn!(profiles_response, ProfileListResponseV1);
schema_fn!(create_profile_request, CreateProfileRequestV1);
schema_fn!(attach_profile_request, AttachProfileRequestV1);
schema_fn!(rebind_profile_request, RebindProfileRequestV1);
schema_fn!(profile_response, ProfileSummaryV1);
schema_fn!(delete_profile_request, DeleteProfileRequestV1);
schema_fn!(restore_profile_request, RestoreProfileRequestV1);
schema_fn!(
    resolve_profile_lifecycles_request,
    ResolveProfileLifecyclesRequestV1
);
schema_fn!(profile_lifecycle_response, ProfileLifecycleResponseV1);
schema_fn!(
    resolve_profile_lifecycles_response,
    ResolveProfileLifecyclesResponseV1
);
schema_fn!(
    remove_profile_authorization_request,
    RemoveProfileAuthorizationRequestV1
);
schema_fn!(
    remove_profile_authorization_response,
    ProfileAuthorizationRemovalResponseV1
);
schema_fn!(register_device_request, RegisterDeviceRequestV1);
schema_fn!(device_response, DeviceSummaryV1);
schema_fn!(devices_response, DeviceListResponseV1);
schema_fn!(
    enrollment_candidates_response,
    EnrollmentCandidatesResponseV1
);
schema_fn!(device_attestation_request, DeviceAttestationRequestV1);
schema_fn!(device_attestation_response, DeviceAttestationResponseV1);
schema_fn!(provision_device_request, ProvisionDeviceRequestV1);
schema_fn!(provision_device_response, ProvisionDeviceResponseV1);
schema_fn!(email_start_request, EmailStartRequestV1);
schema_fn!(email_start_response, EmailStartResponseV1);
schema_fn!(email_verify_request, EmailVerifyRequestV1);
schema_fn!(email_verify_response, EmailVerifyResponseV1);
schema_fn!(email_complete_request, EmailCompleteRequestV1);
schema_fn!(email_complete_response, EmailCompleteResponseV1);
schema_fn!(email_signin_request, EmailSignInRequestV1);
schema_fn!(email_signin_response, EmailSignInResponseV1);
schema_fn!(email_resend_request, EmailResendRequestV1);
schema_fn!(email_resend_response, EmailResendResponseV1);
schema_fn!(email_native_start_request, EmailNativeStartRequestV1);
schema_fn!(email_native_start_response, EmailNativeStartResponseV1);
schema_fn!(
    begin_workspace_cloud_activation_request,
    BeginWorkspaceCloudActivationRequestV1
);
schema_fn!(
    workspace_cloud_activation_challenge,
    WorkspaceCloudActivationChallengeV1
);
schema_fn!(
    complete_workspace_cloud_activation_request,
    CompleteWorkspaceCloudActivationRequestV1
);
schema_fn!(
    mutate_workspace_cloud_request,
    MutateWorkspaceCloudRequestV1
);
schema_fn!(
    workspace_cloud_lifecycle_response,
    WorkspaceCloudLifecycleResponseV1
);

macro_rules! route {
    ($id:ident, $method:ident, $path:literal, $access:ident, $request:ident,
     $response:ident, $location:ident, $summary:literal) => {
        Route {
            id: RouteId::$id,
            method: Method::$method,
            path: $path,
            access: Access::$access,
            request: $request,
            response: $response,
            error: developer_error,
            request_location: RequestLocation::$location,
            summary: $summary,
        }
    };
}

pub const ROUTES: &[Route] = &[
    route!(
        ConfirmAccountCreation,
        Post,
        "/v1/oauth/account/confirm",
        Public,
        creation_request,
        creation_response,
        Body,
        "Confirm creation of a separate Koru account"
    ),
    route!(
        CancelAccountCreation,
        Post,
        "/v1/oauth/account/cancel",
        Public,
        creation_request,
        creation_response,
        Body,
        "Cancel an uncommitted account creation"
    ),
    route!(
        GoogleWebStart,
        Post,
        "/v1/oauth/google/web/start",
        Public,
        web_start_request,
        auth_start_response,
        Body,
        "Start Google authentication for the website"
    ),
    route!(
        GoogleWebCallback,
        Get,
        "/v1/oauth/google/web/callback",
        Public,
        callback_request,
        empty_response,
        Query,
        "Consume Google's website callback"
    ),
    route!(
        WebExchange,
        Post,
        "/v1/oauth/web/exchange",
        Public,
        web_exchange_request,
        web_exchange_response,
        Body,
        "Exchange a one-use website grant"
    ),
    route!(
        GoogleNativeStart,
        Post,
        "/v1/oauth/google/native/start",
        Public,
        native_start_request,
        auth_start_response,
        Body,
        "Start Google authentication for a native client"
    ),
    route!(
        GoogleNativeCallback,
        Get,
        "/v1/oauth/google/native/callback",
        Public,
        callback_request,
        empty_response,
        Query,
        "Consume Google's native callback"
    ),
    route!(
        GoogleNativeTokenStart,
        Post,
        "/v1/oauth/google/native/token/start",
        Public,
        native_token_start_request,
        native_token_start_response,
        Body,
        "Create a nonce for the native Google account sheet"
    ),
    route!(
        GoogleNativeTokenComplete,
        Post,
        "/v1/oauth/google/native/token/complete",
        Public,
        native_token_complete_request,
        native_token_complete_response,
        Body,
        "Resolve a native Google ID token into a recoverable exchange proof"
    ),
    route!(
        NativeExchange,
        Post,
        "/v1/oauth/native/exchange",
        Public,
        native_exchange_request,
        native_exchange_response,
        Body,
        "Exchange a native one-use grant"
    ),
    route!(
        NativeDeviceChallenge,
        Post,
        "/v1/oauth/native/device-challenge",
        Public,
        native_exchange_request,
        device_ownership_challenge_response,
        Body,
        "Issue fresh device ownership evidence for a native exchange"
    ),
    route!(
        DeviceRegistrationChallenge,
        Post,
        "/v1/account/devices/challenge",
        AccountSession,
        register_device_request,
        device_ownership_challenge_response,
        Body,
        "Issue fresh device ownership evidence for account registration"
    ),
    route!(
        Refresh,
        Post,
        "/v1/oauth/refresh",
        Public,
        refresh_request,
        refresh_response,
        Body,
        "Rotate a native refresh token"
    ),
    route!(
        Logout,
        Post,
        "/v1/oauth/logout",
        AccountSession,
        logout_request,
        empty_response,
        Body,
        "Revoke the current account session"
    ),
    route!(
        Session,
        Get,
        "/v1/oauth/session",
        AccountSession,
        empty_request,
        session_response,
        None,
        "Inspect the current account session"
    ),
    route!(
        Account,
        Get,
        "/v1/account",
        AccountSession,
        empty_request,
        account_response,
        None,
        "Inspect the current Koru Identity"
    ),
    route!(
        UpdateAccount,
        Put,
        "/v1/account",
        AccountSession,
        update_account_request,
        account_response,
        Body,
        "Update the current Koru Identity"
    ),
    route!(
        Jwks,
        Get,
        "/v1/oauth/.well-known/jwks.json",
        Public,
        empty_request,
        jwks_response,
        None,
        "Read account token verification keys"
    ),
    route!(
        LinkGoogleStart,
        Post,
        "/v1/oauth/identities/google/start",
        RecentAuthentication,
        link_start_request,
        auth_start_response,
        Body,
        "Start explicit Google identity linking"
    ),
    route!(
        LinkGoogleContinue,
        Post,
        "/v1/oauth/identities/google/continue",
        RecentAuthentication,
        link_continue_request,
        add_google_candidate,
        Body,
        "Inspect a verified Google authorization candidate without linking it"
    ),
    route!(
        ConfirmAddGoogle,
        Post,
        "/v1/oauth/identities/google/confirm",
        RecentAuthentication,
        confirm_add_google_request,
        confirm_add_google_response,
        Body,
        "Confirm the exact Google authorization candidate"
    ),
    route!(
        CancelAddGoogle,
        Post,
        "/v1/oauth/identities/google/cancel",
        RecentAuthentication,
        cancel_add_google_request,
        cancel_add_google_response,
        Body,
        "Cancel the exact Google authorization candidate"
    ),
    route!(
        ListProfiles,
        Get,
        "/v1/account/profiles",
        AccountSession,
        empty_request,
        profiles_response,
        None,
        "List Profiles authorized by this authentication"
    ),
    route!(
        CreateProfile,
        Post,
        "/v1/account/profiles",
        AccountSession,
        create_profile_request,
        profile_response,
        Body,
        "Create a hosted binding for a new local Profile"
    ),
    route!(
        AttachProfile,
        Post,
        "/v1/account/profiles/attach",
        RecentAuthentication,
        attach_profile_request,
        profile_response,
        Body,
        "Attach an existing local Profile"
    ),
    route!(
        RebindProfile,
        Post,
        "/v1/account/profiles/rebind",
        RecentAuthentication,
        rebind_profile_request,
        profile_response,
        Body,
        "Explicitly rebind after server-side erasure"
    ),
    route!(
        RemoveProfileAuthorization,
        Post,
        "/v1/account/profiles/{profile_id}/auth-identities/remove",
        AccountSession,
        remove_profile_authorization_request,
        remove_profile_authorization_response,
        Body,
        "Remove one Auth Identity authorization from one Profile"
    ),
    route!(
        DeleteProfile,
        Delete,
        "/v1/account/profiles/{profile_id}",
        NativeRecentAuthentication,
        delete_profile_request,
        profile_lifecycle_response,
        Body,
        "Permanently remove a Profile from this Koru Identity"
    ),
    route!(
        RestoreProfile,
        Post,
        "/v1/account/profiles/{profile_id}/restore",
        NativeRecentAuthentication,
        restore_profile_request,
        profile_lifecycle_response,
        Body,
        "Explicitly add a deleted Profile to this Koru Identity again"
    ),
    route!(
        ResolveProfileLifecycles,
        Post,
        "/v1/account/profiles/lifecycle",
        NativeDeviceSession,
        resolve_profile_lifecycles_request,
        resolve_profile_lifecycles_response,
        Body,
        "Resolve lifecycle state for a bounded set of exact Profile IDs"
    ),
    route!(
        RegisterDevice,
        Post,
        "/v1/account/devices",
        AccountSession,
        register_device_request,
        device_response,
        Body,
        "Register a signed local device descriptor"
    ),
    route!(
        ListDevices,
        Get,
        "/v1/account/devices",
        AccountSession,
        empty_request,
        devices_response,
        None,
        "List account device discovery metadata"
    ),
    route!(
        ListEnrollmentCandidates,
        Get,
        "/v1/account/profiles/{profile_id}/enrollment-candidates",
        AccountSession,
        empty_request,
        enrollment_candidates_response,
        None,
        "List compatible provisioned enrollment peers for a Profile"
    ),
    route!(
        IssueDeviceAttestation,
        Post,
        "/v1/account/profiles/{profile_id}/device-attestations",
        AccountSession,
        device_attestation_request,
        device_attestation_response,
        Body,
        "Issue a short-lived device enrollment attestation"
    ),
    route!(
        ProvisionDevice,
        Put,
        "/v1/account/profiles/{profile_id}/devices/{device_id}/provisioning",
        AccountSession,
        provision_device_request,
        provision_device_response,
        Body,
        "Record completed local enrollment provisioning metadata"
    ),
    route!(
        BeginWorkspaceCloudActivation,
        Post,
        "/v1/account/workspaces/{workspace_id}/cloud/activation",
        NativeDeviceSession,
        begin_workspace_cloud_activation_request,
        workspace_cloud_activation_challenge,
        Body,
        "Begin a device-key-proven Workspace Cloud activation"
    ),
    route!(
        CompleteWorkspaceCloudActivation,
        Post,
        "/v1/account/workspaces/{workspace_id}/cloud/activation/complete",
        NativeDeviceSession,
        complete_workspace_cloud_activation_request,
        workspace_cloud_lifecycle_response,
        Body,
        "Complete a device-key-proven Workspace Cloud activation"
    ),
    route!(
        DisableWorkspaceCloud,
        Post,
        "/v1/account/workspaces/{workspace_id}/cloud/disable",
        NativeDeviceSession,
        mutate_workspace_cloud_request,
        workspace_cloud_lifecycle_response,
        Body,
        "Turn off Cloud sync while retaining its copy"
    ),
    route!(
        DeleteWorkspaceCloud,
        Delete,
        "/v1/account/workspaces/{workspace_id}/cloud",
        NativeDeviceSession,
        mutate_workspace_cloud_request,
        workspace_cloud_lifecycle_response,
        Body,
        "Begin deletion of one Workspace Cloud copy"
    ),
    route!(
        InspectNativeSession,
        Post,
        "/v1/oauth/native/session/inspect",
        Public,
        native_session_inspect_request,
        session_response,
        Body,
        "Observe current native credential ownership without changing the session"
    ),
    route!(
        EmailStart,
        Post,
        "/v1/oauth/email/start",
        Public,
        email_start_request,
        email_start_response,
        Body,
        "Begin proving an email address"
    ),
    route!(
        EmailVerify,
        Post,
        "/v1/oauth/email/verify",
        Public,
        email_verify_request,
        email_verify_response,
        Body,
        "Check a mailed verification code"
    ),
    route!(
        EmailComplete,
        Post,
        "/v1/oauth/email/complete",
        Public,
        email_complete_request,
        email_complete_response,
        Body,
        "Choose a password and create the account"
    ),
    route!(
        EmailSignIn,
        Post,
        "/v1/oauth/email/signin",
        Public,
        email_signin_request,
        email_signin_response,
        Body,
        "Sign in with an email address and password"
    ),
    route!(
        EmailResend,
        Post,
        "/v1/oauth/email/resend",
        Public,
        email_resend_request,
        email_resend_response,
        Body,
        "Send another verification code"
    ),
    route!(
        EmailNativeStart,
        Post,
        "/v1/oauth/email/native/start",
        Public,
        email_native_start_request,
        email_native_start_response,
        Body,
        "Start email authentication for a native client"
    ),
    route!(
        IssueCloudSyncToken,
        Post,
        "/v1/account/workspaces/{workspace_id}/cloud/sync-token",
        NativeDeviceSession,
        sync_token_request,
        sync_token_response,
        Body,
        "Obtain a short-lived credential for exact Workspace and Profile Cloud sync"
    ),
    route!(
        ListCloudWorkspaces,
        Get,
        "/v1/account/cloud/workspaces",
        NativeDeviceSession,
        empty_request,
        cloud_workspace_page,
        None,
        "List one bounded page of currently authorized Cloud Workspace eligibility pairs"
    ),
    route!(
        SelectProfileCloud,
        Put,
        "/v1/account/profiles/{profile_id}/cloud/selection",
        NativeDeviceSession,
        cloud_profile_selection_request,
        cloud_profile_selection_response,
        Body,
        "Select one exact Profile and Workspace pair for Cloud sync on this device"
    ),
    route!(
        DescribeCloudWorkspace,
        Get,
        "/v1/account/workspaces/{workspace_id}/cloud/descriptor",
        NativeDeviceSession,
        empty_request,
        cloud_workspace_descriptor,
        None,
        "Read the identity-only bootstrap descriptor of one authorized Cloud Workspace"
    ),
    route!(
        InspectWorkspaceCloud,
        Get,
        "/v1/account/workspaces/{workspace_id}/cloud/state",
        NativeDeviceSession,
        empty_request,
        workspace_cloud_lifecycle_response,
        None,
        "Read one Workspace's current Cloud lifecycle state and authorization version, whatever its state"
    ),
    // Sign in with Apple. Appended: `route_by_id` indexes this table by
    // `RouteId` position, so the table and the enum grow together at the end.
    route!(
        AppleWebStart,
        Post,
        "/v1/oauth/apple/web/start",
        Public,
        web_start_request,
        auth_start_response,
        Body,
        "Start Sign in with Apple for the website"
    ),
    route!(
        AppleWebCallback,
        Post,
        "/v1/oauth/apple/web/callback",
        Public,
        apple_callback_request,
        empty_response,
        Form,
        "Consume Apple's website form_post callback"
    ),
    route!(
        AppleNativeStart,
        Post,
        "/v1/oauth/apple/native/start",
        Public,
        native_start_request,
        auth_start_response,
        Body,
        "Start Sign in with Apple for a native client"
    ),
    route!(
        AppleNativeCallback,
        Post,
        "/v1/oauth/apple/native/callback",
        Public,
        apple_callback_request,
        empty_response,
        Form,
        "Consume Apple's native form_post callback"
    ),
    // App connections (Cloud MCP §9): the consent page's two calls and the
    // connections list with its edits. The OAuth protocol routes themselves
    // are `app_connections::OAUTH_ROUTES`, outside this account catalog.
    route!(
        OAuthConsentRead,
        Get,
        "/v1/oauth/consent",
        WebSession,
        oauth_consent_query,
        oauth_consent_view,
        Query,
        "Read what the consent page shows for a pending authorization request"
    ),
    route!(
        OAuthConsentDecide,
        Post,
        "/v1/oauth/consent",
        WebSession,
        oauth_consent_decision_request,
        oauth_consent_decision_response,
        Body,
        "Approve or deny a pending authorization request"
    ),
    route!(
        ListAppConnections,
        Get,
        "/v1/account/connections",
        AccountSession,
        empty_request,
        app_connection_list,
        None,
        "List the account's connected assistants, live and revoked"
    ),
    route!(
        UpdateAppConnection,
        Patch,
        "/v1/account/connections/{connection_id}",
        AccountSession,
        update_app_connection_request,
        app_connection_summary,
        Body,
        "Rename a connection or change its Profiles, later-Profiles flag or access"
    ),
    route!(
        RevokeAppConnection,
        Delete,
        "/v1/account/connections/{connection_id}",
        AccountSession,
        empty_request,
        app_connection_summary,
        None,
        "Revoke a connection"
    ),
];

schema_fn!(
    oauth_consent_query,
    crate::app_connections::OAuthConsentQueryV1
);
schema_fn!(
    oauth_consent_view,
    crate::app_connections::OAuthConsentViewV1
);
schema_fn!(
    oauth_consent_decision_request,
    crate::app_connections::OAuthConsentDecisionRequestV1
);
schema_fn!(
    oauth_consent_decision_response,
    crate::app_connections::OAuthConsentDecisionResponseV1
);
schema_fn!(
    app_connection_list,
    crate::app_connections::AppConnectionListResponseV1
);
schema_fn!(
    app_connection_summary,
    crate::app_connections::AppConnectionSummaryV1
);
schema_fn!(
    update_app_connection_request,
    crate::app_connections::UpdateAppConnectionRequestV1
);

schema_fn!(sync_token_request, SyncTokenRequestV1);
schema_fn!(sync_token_response, SyncTokenResponseV1);
schema_fn!(cloud_workspace_page, CloudWorkspacePageV1);
schema_fn!(cloud_workspace_descriptor, CloudWorkspaceDescriptorV1);
schema_fn!(
    cloud_profile_selection_request,
    CloudProfileSelectionRequestV1
);
schema_fn!(
    cloud_profile_selection_response,
    CloudProfileSelectionResponseV1
);

pub fn route_by_id(id: RouteId) -> &'static Route {
    &ROUTES[id as usize]
}

/// Finds the frozen account operation for an HTTP method and path. Braced
/// catalog segments match exactly one non-empty segment; decoding and typed
/// validation remain the HTTP adapter's responsibility.
///
/// The API adapter, client contract tests, gateway admission exception, and
/// generated documentation all use this lookup rather than maintaining a
/// second account-route list.
pub fn route_for(method: Method, path: &str) -> Option<&'static Route> {
    ROUTES
        .iter()
        .find(|route| route.method == method && path_matches(route.path, path))
}

pub fn is_route_path(path: &str) -> bool {
    ROUTES.iter().any(|route| path_matches(route.path, path))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.split('/');
    let mut path = path.split('/');
    loop {
        match (pattern.next(), path.next()) {
            (Some(expected), Some(actual))
                if (expected.starts_with('{') && expected.ends_with('}') && !actual.is_empty())
                    || expected == actual => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}
