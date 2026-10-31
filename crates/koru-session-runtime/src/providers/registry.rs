// SPDX-License-Identifier: MPL-2.0

use super::{
    ManagedProvider, ManagedProviderFactory, ManagedProviderRequest, ProviderDiagnostic,
    ProviderFailure, ProviderStage,
};
use koru_session_profiles::ManagedProviderRegistration;
use koru_session_protocol::{CapabilityName, ErrorCode, ProviderKind, capability};
use std::collections::{BTreeMap, btree_map::Entry};
use std::str::FromStr;
use std::sync::Arc;

/// Host-owned dispatch table built before sessions are admitted. Each create
/// returns its own control; this table knows no run ownership and never
/// substitutes for a session's capabilities or live delegation verdict.
#[derive(Clone, Default)]
pub struct ManagedProviderRegistry {
    entries: BTreeMap<ProviderKind, RegisteredProvider>,
}

#[derive(Clone)]
struct RegisteredProvider {
    registration: ManagedProviderRegistration,
    factory: Arc<dyn ManagedProviderFactory>,
}

impl ManagedProviderRegistry {
    /// Duplicate kinds are refused without replacing the installed factory.
    pub fn register(
        &mut self,
        registration: ManagedProviderRegistration,
        factory: Arc<dyn ManagedProviderFactory>,
    ) -> Result<(), ProviderFailure> {
        let invalid = || {
            ProviderFailure::new(
                ErrorCode::InvalidRequest,
                ProviderStage::Discovery,
                ProviderDiagnostic::InvalidInput,
            )
        };
        let kind = ProviderKind::from_str(registration.kind).map_err(|_| invalid())?;
        CapabilityName::from_str(registration.minimum_host_capability).map_err(|_| invalid())?;
        if registration.executable.is_empty()
            || !capability::ALL.contains(&registration.minimum_host_capability)
        {
            return Err(invalid());
        }
        match self.entries.entry(kind) {
            Entry::Vacant(entry) => {
                entry.insert(RegisteredProvider {
                    registration,
                    factory,
                });
                Ok(())
            }
            Entry::Occupied(_) => Err(invalid()),
        }
    }

    pub fn registration(&self, kind: &ProviderKind) -> Option<&ManagedProviderRegistration> {
        self.entries.get(kind).map(|entry| &entry.registration)
    }

    /// A cached read starts no discovery and performs no I/O.
    pub fn registrations(&self) -> impl Iterator<Item = &ManagedProviderRegistration> {
        self.entries.values().map(|entry| &entry.registration)
    }

    pub fn compatibility_probe(
        &self,
        kind: &ProviderKind,
        working_directory: std::path::PathBuf,
    ) -> Result<Box<dyn super::CompatibilityProbe>, ProviderFailure> {
        let entry = self.entries.get(kind).ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Discovery,
                ProviderDiagnostic::VersionRejected,
            )
        })?;
        entry
            .factory
            .compatibility_probe(entry.registration.executable.into(), working_directory)
    }

    /// Where the model catalog gets `kind`'s list; `None` for a kind that
    /// is not registered.
    pub fn catalog_source(&self, kind: &ProviderKind) -> Option<super::CatalogSource> {
        self.entries
            .get(kind)
            .map(|entry| entry.factory.catalog_source())
    }

    pub fn discover_models(
        &self,
        kind: &ProviderKind,
        working_directory: std::path::PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        let entry = self.entries.get(kind).ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Discovery,
                ProviderDiagnostic::HandshakeRejected,
            )
        })?;
        entry
            .factory
            .discover_models(entry.registration.executable.into(), working_directory)
    }

    pub fn create(
        &self,
        request: ManagedProviderRequest,
    ) -> Result<ManagedProvider, ProviderFailure> {
        let entry = self.entries.get(&request.provider).ok_or_else(|| {
            // Preserve the production Codex factory's refusal for other kinds.
            ProviderFailure::new(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            )
        })?;
        if request
            .delegation
            .as_ref()
            .is_some_and(|launch| launch.model.is_some())
            && !entry.factory.supports_model_selection()
        {
            return Err(ProviderFailure::new(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        entry.factory.create(request)
    }
}

#[cfg(test)]
mod tests;
