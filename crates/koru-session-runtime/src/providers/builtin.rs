// SPDX-License-Identifier: MPL-2.0

use super::{AcpFactory, AcpRoute, CodexFactory, ManagedProviderRegistry};
use crate::AttachmentPathResolver;
use std::sync::Arc;

/// The same reviewed provider registrations for every process that directly
/// owns managed providers. Constructing the table performs no discovery or I/O.
pub fn builtin_providers(paths: Arc<dyn AttachmentPathResolver>) -> ManagedProviderRegistry {
    let mut registry = ManagedProviderRegistry::default();
    registry
        .register(
            koru_session_profiles::CODEX_REGISTRATION,
            Arc::new(CodexFactory::new(paths.clone())),
        )
        .expect("built-in provider registrations are valid and unique");
    for (registration, route) in [
        (
            koru_session_profiles::CLAUDE_REGISTRATION,
            AcpRoute::claude_code(),
        ),
        (
            koru_session_profiles::OPENCODE_REGISTRATION,
            AcpRoute::opencode(),
        ),
        (koru_session_profiles::KIMI_REGISTRATION, AcpRoute::kimi()),
        (koru_session_profiles::GROK_REGISTRATION, AcpRoute::grok()),
        (
            koru_session_profiles::ANTIGRAVITY_REGISTRATION,
            AcpRoute::antigravity(),
        ),
        (koru_session_profiles::DEVIN_REGISTRATION, AcpRoute::devin()),
    ] {
        registry
            .register(
                registration,
                Arc::new(AcpFactory::new(route, paths.clone())),
            )
            .expect("built-in provider registrations are valid and unique");
    }
    registry
}

#[cfg(test)]
mod tests;
