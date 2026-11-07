// SPDX-License-Identifier: MPL-2.0

use koru_account_core::RateLimitScope;
use serde::{Deserialize, Serialize};

pub const ACCESS_TOKEN_MILLIS: i64 = 15 * 60 * 1_000;
pub const REFRESH_IDLE_MILLIS: i64 = 90 * 24 * 60 * 60 * 1_000;
pub const REFRESH_ABSOLUTE_MILLIS: i64 = 365 * 24 * 60 * 60 * 1_000;
pub const RETRY_MILLIS: i64 = 60 * 1_000;
pub const FLOW_MILLIS: i64 = 10 * 60 * 1_000;
pub const SECRET_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolicReturnTarget {
    Account,
    Releases,
    DownloadWindows,
    DownloadMacos,
    /// The website's app connection consent page (Cloud MCP TECHNICAL.final
    /// §7.3): a sign-in started there lands back on it, where the pending
    /// request waits in its own cookie.
    ConnectionAuthorize,
}
impl SymbolicReturnTarget {
    pub fn parse(value: &str) -> Result<Self, SecurityError> {
        match value {
            "account" => Ok(Self::Account),
            "releases" => Ok(Self::Releases),
            "download_windows" => Ok(Self::DownloadWindows),
            "download_macos" => Ok(Self::DownloadMacos),
            "connection_authorize" => Ok(Self::ConnectionAuthorize),
            _ => Err(SecurityError::InvalidReturnTarget),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRedirectPolicy {
    Disabled,
}
pub const PROVIDER_REDIRECT_POLICY: ProviderRedirectPolicy = ProviderRedirectPolicy::Disabled;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityError {
    InvalidReturnTarget,
    InvalidOrigin,
    InvalidCsrf,
    Expired,
    Replayed,
    RateLimited,
    InvalidSecret,
    Repository,
    Crypto,
}
impl std::fmt::Display for SecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidReturnTarget => "return target is not allowed",
            Self::InvalidOrigin => "request origin is not allowed",
            Self::InvalidCsrf => "request CSRF proof is invalid",
            Self::Expired => "the security artifact expired",
            Self::Replayed => "the security artifact was already used",
            Self::RateLimited => "the request rate limit was exceeded",
            Self::InvalidSecret => "the security proof is invalid",
            Self::Repository => "the security repository is unavailable",
            Self::Crypto => "the security primitive is unavailable",
        })
    }
}
impl std::error::Error for SecurityError {}

pub fn validate_web_request(
    origin: Option<&str>,
    allowed_origin: &str,
    cookie_csrf: &[u8],
    header_csrf: &[u8],
) -> Result<(), SecurityError> {
    if origin != Some(allowed_origin) {
        return Err(SecurityError::InvalidOrigin);
    }
    if cookie_csrf.is_empty() || !constant_time_eq(cookie_csrf, header_csrf) {
        return Err(SecurityError::InvalidCsrf);
    }
    Ok(())
}
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatePolicy {
    pub scope: RateLimitScope,
    pub limit: u32,
    pub window_millis: u64,
}
pub const START_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::AuthStart,
    limit: 20,
    window_millis: 600_000,
};
pub const CALLBACK_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::CallbackOrExchange,
    limit: 10,
    window_millis: 600_000,
};
pub const AUTHENTICATION_ATTEMPT_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::AuthenticationAttempt,
    limit: 30,
    window_millis: 600_000,
};
pub const LINK_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::IdentityLink,
    limit: 5,
    window_millis: 900_000,
};
pub const REFRESH_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::Refresh,
    limit: 30,
    window_millis: 600_000,
};
/// Twenty was spent by a developer restarting: every Agent start runs the
/// profile-lifecycle reconcile and the enrollment attestation, and a day
/// of builds tripped it four times, each one parking the whole account
/// behind a retry. Two hundred keeps a runaway client bounded and leaves
/// a person's restarts alone (maintainers, 2026-09-24).
pub const DEVICE_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::DeviceOperation,
    limit: 200,
    window_millis: 600_000,
};

/// Email sign-in's four budgets, and what each one is protecting.
///
/// The send pair is what stops our own resend button being used to flood a
/// stranger's inbox: a source may start a reasonable number of sign-ups, and
/// any single address may receive far fewer, because the person being
/// mailed did not ask for any of them.
pub const EMAIL_CHALLENGE_SEND_SOURCE_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::EmailChallengeSend,
    limit: 20,
    window_millis: 600_000,
};
pub const EMAIL_CHALLENGE_SEND_ADDRESS_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::EmailChallengeSend,
    limit: 5,
    window_millis: 600_000,
};

/// Per-challenge `attempts` already bounds guesses at five, but only within
/// one challenge. This bounds an attacker who starts many flows and spends a
/// few guesses on each, which that counter cannot see.
pub const EMAIL_CHALLENGE_VERIFY_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::EmailChallengeVerify,
    limit: 60,
    window_millis: 600_000,
};

/// One host spraying passwords at many accounts.
pub const EMAIL_PASSWORD_SOURCE_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::EmailPasswordSourceAttempt,
    limit: 30,
    window_millis: 600_000,
};

/// One account being targeted from many hosts. Tighter and over a longer
/// window than the source budget, because the person whose account it is
/// suffers this one and did nothing to invite it.
pub const EMAIL_PASSWORD_ADDRESS_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::EmailPasswordAddressAttempt,
    limit: 5,
    window_millis: 900_000,
};

/// Independent sync issuance allowance, not a new persisted rate scope.
pub const CLOUD_SYNC_TOKEN_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::DeviceOperation,
    limit: 30,
    window_millis: 60_000,
};

/// Background enrollment stages have independent per-account/device budgets.
/// They must not consume each other's allowance or the sensitive mutation one.
pub const ENROLLMENT_RATE: RatePolicy = RatePolicy {
    scope: RateLimitScope::DeviceOperation,
    limit: 120,
    window_millis: 60_000,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollmentRateOperation {
    Discovery,
    Attestation,
    Provisioning,
}

impl EnrollmentRateOperation {
    pub(crate) fn purpose(self) -> &'static str {
        match self {
            Self::Discovery => "rate-enrollment-discovery",
            Self::Attestation => "rate-enrollment-attestation",
            Self::Provisioning => "rate-enrollment-provisioning",
        }
    }
}

pub enum RateLimitSubject<'a> {
    CloudSyncToken {
        identity: koru_account_core::KoruIdentityId,
        device: koru_account_core::DeviceId,
        workspace: koru_account_core::WorkspaceId,
    },
    Enrollment {
        identity: koru_account_core::KoruIdentityId,
        device: koru_account_core::DeviceId,
        operation: EnrollmentRateOperation,
    },
    AuthStart {
        source: &'a [u8],
    },
    AuthenticationAttempt {
        source: &'a [u8],
    },
    CallbackOrExchange {
        flow: koru_account_core::AuthFlowId,
        source: &'a [u8],
    },
    IdentityLink {
        flow: Option<koru_account_core::AuthFlowId>,
        identity: Option<koru_account_core::KoruIdentityId>,
    },
    Refresh {
        session: koru_account_core::AccountSessionId,
    },
    DeviceOperation {
        identity: koru_account_core::KoruIdentityId,
        device: koru_account_core::DeviceId,
    },
    /// Email sign-in's five. Sending and password attempts are each bounded
    /// twice -- once by where the request came from and once by whose address
    /// it names -- because the two budgets protect different people. A source
    /// budget stops one host abusing many addresses; an address budget stops
    /// many hosts abusing one person, who did nothing to invite it.
    EmailChallengeSendSource {
        source: &'a [u8],
    },
    EmailChallengeSendAddress {
        address: &'a koru_account_core::KeyedDigest,
    },
    EmailChallengeVerify {
        source: &'a [u8],
    },
    EmailPasswordSource {
        source: &'a [u8],
    },
    EmailPasswordAddress {
        address: &'a koru_account_core::KeyedDigest,
    },
}
