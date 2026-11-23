// SPDX-License-Identifier: MPL-2.0

//! Cloud access and purchase management are separate account decisions.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Page {
    subscriptions: Vec<Entry>,
    /// Older deployments omit this; retain their subscription-based behavior.
    #[serde(default)]
    cloud_access: Option<bool>,
    #[serde(deserialize_with = "required_cursor")]
    next_cursor: Option<String>,
}

/// Optional onboarding A/B variant codes forwarded on checkout handoff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckoutHandoffAb<'a> {
    pub code: Option<&'a str>,
    pub header: Option<&'a str>,
    pub billing: Option<&'a str>,
}

impl CheckoutHandoffAb<'_> {
    pub fn is_empty(self) -> bool {
        self.code.is_none() && self.header.is_none() && self.billing.is_none()
    }
}

#[derive(Serialize)]
struct CheckoutHandoffRequest<'a> {
    plan: &'a str,
    locale: &'a str,
    callback_port: u16,
    client_state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    billing: Option<&'a str>,
}

#[derive(Deserialize)]
struct CheckoutHandoffResponse {
    checkout_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudSubscriptionStatus {
    pub current: bool,
    pub cloud_access: bool,
    pub kind: Option<SubscriptionKind>,
}
fn required_cursor<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(value)
}
#[derive(Deserialize)]
struct Entry {
    status: Status,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Incomplete,
    IncompleteExpired,
    Trialing,
    Active,
    PastDue,
    Canceled,
    Unpaid,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionKind {
    Trialing,
    Active,
    PastDue,
    Unpaid,
    Paused,
    Incomplete,
    Complimentary,
}

impl Status {
    fn as_kind(self) -> Option<SubscriptionKind> {
        match self {
            Self::Trialing => Some(SubscriptionKind::Trialing),
            Self::Active => Some(SubscriptionKind::Active),
            Self::PastDue => Some(SubscriptionKind::PastDue),
            Self::Unpaid => Some(SubscriptionKind::Unpaid),
            Self::Paused => Some(SubscriptionKind::Paused),
            Self::Incomplete => Some(SubscriptionKind::Incomplete),
            Self::Canceled | Self::IncompleteExpired => None,
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Active => 6,
            Self::Trialing => 5,
            Self::PastDue => 4,
            Self::Unpaid => 3,
            Self::Paused => 2,
            Self::Incomplete => 1,
            Self::Canceled | Self::IncompleteExpired => 0,
        }
    }
}

impl AccountClient {
    /// Existing nonterminal purchases go to management, even when access is
    /// unpaid or pending cancellation. Only a complete traversal proves absence.
    /// Call on the account worker; cancellation is checked between bounded HTTP
    /// requests. Exhaustion, malformed/repeating cursors and outages stay unknown.
    pub fn has_cloud_subscription(
        &self,
        authorization: &AccountAuthorization<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, ClientError> {
        self.cloud_subscription_status(authorization, cancelled)
            .map(|status| status.current)
    }

    /// Read current access independently of whether there is a purchase to manage,
    /// along with the subscription kind for UI status labels.
    /// Complimentary access never fabricates a subscription entry.
    pub fn cloud_subscription_status(
        &self,
        authorization: &AccountAuthorization<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CloudSubscriptionStatus, ClientError> {
        let started = std::time::Instant::now();
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        let mut best: Option<Status> = None;
        let mut saw_cloud_access: Option<bool> = None;
        for _ in 0..32 {
            if cancelled() || started.elapsed() >= REQUEST_TIMEOUT {
                return Err(ClientError::Transport(TransportError::Timeout));
            }
            let mut path = "/v1/account/subscription?limit=25".to_owned();
            if let Some(cursor) = &cursor {
                path.push_str("&cursor=");
                path.push_str(cursor);
            }
            // This separately composed billing read shares the ordinary account
            // GET's auth and bounded transport policy, including diagnostics.
            let page: Page = self.execute(
                RouteId::Account,
                vec![],
                Some(authorization),
                false,
                Some(path),
            )?;
            if cancelled() || started.elapsed() >= REQUEST_TIMEOUT {
                return Err(ClientError::Transport(TransportError::Timeout));
            }
            if page.subscriptions.len() > 25 {
                return Err(ClientError::InvalidResponse);
            }
            if page.cloud_access.is_some() {
                saw_cloud_access = page.cloud_access;
            }
            for entry in &page.subscriptions {
                if entry.status.priority() > best.map_or(0, Status::priority) {
                    best = Some(entry.status);
                }
            }
            if let Some(status) = best
                && status.as_kind().is_some()
            {
                return Ok(CloudSubscriptionStatus {
                    current: true,
                    cloud_access: page.cloud_access.or(saw_cloud_access).unwrap_or(true),
                    kind: status.as_kind(),
                });
            }
            let Some(next) = page.next_cursor else {
                let cloud_access = page.cloud_access.or(saw_cloud_access).unwrap_or(false);
                return Ok(CloudSubscriptionStatus {
                    current: false,
                    cloud_access,
                    kind: cloud_access.then_some(SubscriptionKind::Complimentary),
                });
            };
            if next.is_empty()
                || next.len() > 2048
                || !next
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !seen.insert(next.clone())
            {
                return Err(ClientError::InvalidResponse);
            }
            cursor = Some(next);
        }
        Err(ClientError::InvalidResponse)
    }

    /// Whether the account currently has active Cloud access (trialing, active,
    /// or complimentary grant) as reported by `/v1/account/subscription`.
    pub fn cloud_subscription_access(
        &self,
        authorization: &AccountAuthorization<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, ClientError> {
        self.cloud_subscription_status(authorization, cancelled)
            .map(|status| status.cloud_access)
    }

    /// Mint a short-lived signed desktop checkout handoff URL on the website so
    /// Stripe Embedded Checkout locks to the account's verified email and
    /// redirects back to the local loopback listener when complete.
    pub fn create_checkout_handoff(
        &self,
        authorization: &AccountAuthorization<'_>,
        plan: &str,
        locale: &str,
        callback_port: u16,
        client_state: &str,
        ab: CheckoutHandoffAb<'_>,
    ) -> Result<String, ClientError> {
        if !matches!(plan, "s1" | "y1") || callback_port == 0 || client_state.is_empty() {
            return Err(ClientError::InvalidRequest);
        }
        let send = |ab: CheckoutHandoffAb<'_>| -> Result<CheckoutHandoffResponse, ClientError> {
            let body = serde_json::to_vec(&CheckoutHandoffRequest {
                plan,
                locale,
                callback_port,
                client_state,
                code: ab.code,
                header: ab.header,
                billing: ab.billing,
            })
            .map_err(|_| ClientError::InvalidRequest)?;
            self.execute(
                RouteId::CreateProfile,
                body,
                Some(authorization),
                false,
                Some("/v1/billing/stripe/checkout-handoff".to_owned()),
            )
        };
        let response = match send(ab) {
            Ok(response) => response,
            // Older deployments enforce `deny_unknown_fields` on checkout-handoff
            // and return `{"error":{"code":"invalid_request"}}` (HTTP 400, which
            // decodes as `ApiWithoutCode { status: 400 }`) or `DeveloperErrorV1`
            // (`Api { status: 400, .. }`). Fall back cleanly once without the
            // optional A/B metadata so checkout still completes against older servers.
            Err(
                ClientError::Api { status: 400, .. }
                | ClientError::ApiWithoutCode { status: 400 },
            ) if !ab.is_empty() => send(CheckoutHandoffAb::default())?,
            Err(err) => return Err(err),
        };
        if response.checkout_url.is_empty() || response.checkout_url.len() > 8192 {
            return Err(ClientError::InvalidResponse);
        }
        Ok(response.checkout_url)
    }
}

#[cfg(test)]
mod tests;
