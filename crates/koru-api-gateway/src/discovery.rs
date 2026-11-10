// SPDX-License-Identifier: MPL-2.0

//! The public rendezvous: who is admitted, what they are told, and what a
//! vouched scope costs.
//!
//! Binding and serving are deliberately two steps, and the gap between them is
//! ordering (2). [`bind`] happens **before Store opens**, because the endpoint
//! name carries the root and holding it is how two gateway images are stopped
//! from racing into two owners for one root. `Rendezvous::serve` is spawned
//! last, once the composition is up and the listener has an address — a record
//! that is published is a promise that the endpoint behind it answers.

use std::sync::Arc;

use koru_api_discovery::{ClientHello, GatewayHello, GatewayResponse, SessionCredential};
use koru_api_gateway_core::{
    Admission, GatewayState, WORKSPACE_DESTROY_SCOPE, WORKSPACE_MIGRATE_SCOPE, now_millis,
};

/// **Ordering (2): claim the root before Store opens.**
///
/// The endpoint is qualified by the channel root, so a second image on the same
/// root fails here — with no Store opened, no journal touched and nothing to
/// unwind.
pub(crate) fn bind(
    segment: &str,
    channel_root: &std::path::Path,
) -> Result<koru_api_discovery::server::Rendezvous, String> {
    koru_api_discovery::server::Rendezvous::bind_at(segment, channel_root)
        .map_err(|error| error.to_string())
}

/// What every accepted caller is told, and what a vouched one has to prove.
pub(crate) struct Policy {
    pub(crate) state: Arc<GatewayState>,
    pub(crate) segment: String,
    pub(crate) gateway_version: String,
    pub(crate) endpoint: String,
    pub(crate) instance_id: String,
    pub(crate) supervisor_launch_id: Option<String>,
    /// Where to ask the supervisor about a presented grant. `None` when this
    /// gateway resolved no Agent endpoint, which makes every caller ordinary.
    pub(crate) agent: Option<koru_ipc::Endpoint>,
}

impl koru_api_discovery::server::Policy for Policy {
    fn greet(&self, hello: &ClientHello) -> GatewayResponse {
        if let Some(refusal) =
            koru_api_discovery::admission_refusal(hello, &self.segment, koru_api_wire::API_VERSION)
        {
            return refusal;
        }
        let admission = self.admit(hello);
        let (bearer, expires_at_millis) = self.state.sessions.mint_admitted(
            &hello.product,
            &hello.requested_scopes,
            admission,
            now_millis(),
        );
        GatewayResponse::Welcome(Box::new(GatewayHello {
            api_version: koru_api_wire::API_VERSION,
            supervisor_launch_id: self.supervisor_launch_id.clone(),
            gateway_version: self.gateway_version.clone(),
            segment: self.segment.clone(),
            api_endpoint: self.endpoint.clone(),
            instance_id: self.instance_id.clone(),
            capabilities: self.state.public_capabilities(),
            session: SessionCredential::new(bearer, expires_at_millis),
        }))
    }
}

impl Policy {
    /// **Ask the supervisor, and only when it would change anything.**
    ///
    /// The round trip happens for one reason: a caller named one of the vouched
    /// scopes *and* presented a grant. Every other hello — which
    /// is every hello there has ever been — is ordinary without touching the
    /// Agent, so admission stays exactly as fast as it was and a stopped Agent
    /// never keeps a window from opening.
    ///
    /// **Fails closed by construction.** `grants::validate` answers `false` for
    /// anything that is not a live token: no Agent, a timeout, a framing
    /// mismatch, a spent grant. A destructive permission is never granted
    /// because a check could not be made.
    fn admit(&self, hello: &ClientHello) -> Admission {
        // **Every vouched scope, not just the first one.** This named only
        // `workspace.destroy` and worked because the manager asks for all of
        // them together — a window naming only `workspace.migrate` was admitted
        // ordinary and lost the scope it asked for, silently. A third scope made
        // that a bug waiting rather than a latent one, so the predicate now
        // names the set `effective_scopes` gates on.
        let asks = asks_vouched_scope(&hello.requested_scopes);
        let (Some(agent), Some(grant), true) = (self.agent.as_ref(), hello.grant.clone(), asks)
        else {
            return Admission::Ordinary;
        };
        if koru_api_client::grants::validate(agent, grant) {
            Admission::Vouched
        } else {
            Admission::Ordinary
        }
    }
}

/// This run's correlation id.
///
/// The instance id is correlation state, not document or account identity.
/// Reusing the bearer entropy source also keeps this executable leaf small.
pub(crate) fn instance_id() -> String {
    format!("{}-{}", std::process::id(), now_millis())
}

fn asks_vouched_scope(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|s| s == WORKSPACE_DESTROY_SCOPE || s == WORKSPACE_MIGRATE_SCOPE || s == "bots.manage")
}
#[cfg(test)]
mod tests;
