// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::NoopAttachmentPathResolver;
use crate::providers::CodexFactory;
use koru_session_profiles::CODEX_REGISTRATION;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingFactory {
    calls: Arc<AtomicUsize>,
    expected_kind: &'static str,
}

impl ManagedProviderFactory for CountingFactory {
    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        assert_eq!(request.provider.as_str(), self.expected_kind);
        assert_eq!(request.executable, "fixture");
        assert_eq!(
            request.working_directory,
            std::path::Path::new("fixture-cwd")
        );
        assert!(request.delegation.is_none());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Initialize,
            ProviderDiagnostic::HandshakeRejected,
        ))
    }
}

fn request(kind: &str) -> ManagedProviderRequest {
    ManagedProviderRequest {
        provider: ProviderKind::from_str(kind).unwrap(),
        executable: "fixture".into(),
        working_directory: "fixture-cwd".into(),
        delegation: None,
        selection: None,
        mcp_servers: Vec::new(),
        questions: false,
    }
}

fn add(registry: &mut ManagedProviderRegistry, kind: &'static str) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(
            ManagedProviderRegistration {
                kind,
                ..CODEX_REGISTRATION
            },
            Arc::new(CountingFactory {
                calls: calls.clone(),
                expected_kind: kind,
            }),
        )
        .unwrap();
    calls
}

#[test]
fn two_entries_dispatch_independently_and_reads_do_not_start_work() {
    let mut registry = ManagedProviderRegistry::default();
    let first = add(&mut registry, "codex");
    let second = add(&mut registry, "fixture-agent");
    assert_eq!(registry.registrations().count(), 2);
    assert!(registry.registration(&request("codex").provider).is_some());
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 0);
    assert!(registry.create(request("fixture-agent")).is_err());
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    assert!(registry.create(request("codex")).is_err());
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
}

#[test]
fn duplicate_registration_refuses_without_replacing_the_first() {
    let mut registry = ManagedProviderRegistry::default();
    let original = add(&mut registry, "codex");
    let replacement = Arc::new(AtomicUsize::new(0));
    let error = registry
        .register(
            CODEX_REGISTRATION,
            Arc::new(CountingFactory {
                calls: replacement.clone(),
                expected_kind: "codex",
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(error.stage, ProviderStage::Discovery);
    assert_eq!(registry.registrations().count(), 1);
    assert!(registry.create(request("codex")).is_err());
    assert_eq!(original.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.load(Ordering::SeqCst), 0);
}

#[test]
fn unregistered_kind_preserves_codex_refusal_code_stage_and_diagnostic() {
    let codex = Arc::new(CodexFactory::new(Arc::new(NoopAttachmentPathResolver)));
    // Both refuse before touching the executable: no CLI or process is used.
    let before = codex.create(request("unregistered")).err().unwrap();
    let mut registry = ManagedProviderRegistry::default();
    registry.register(CODEX_REGISTRATION, codex).unwrap();
    let after = registry.create(request("unregistered")).err().unwrap();
    assert_eq!(after, before);
    assert_eq!(after.code, ErrorCode::ProviderIncompatible);
    assert_eq!(after.stage, ProviderStage::Version);
    assert_eq!(after.diagnostic, ProviderDiagnostic::VersionRejected);
}

#[test]
fn invalid_registration_is_never_published() {
    let mut registry = ManagedProviderRegistry::default();
    for registration in [
        ManagedProviderRegistration {
            kind: "invalid kind",
            ..CODEX_REGISTRATION
        },
        ManagedProviderRegistration {
            executable: "",
            ..CODEX_REGISTRATION
        },
        ManagedProviderRegistration {
            minimum_host_capability: "unknown-capability",
            ..CODEX_REGISTRATION
        },
    ] {
        assert!(
            registry
                .register(
                    registration,
                    Arc::new(CodexFactory::new(Arc::new(NoopAttachmentPathResolver))),
                )
                .is_err()
        );
    }
    assert_eq!(registry.registrations().count(), 0);
}

#[test]
fn explicit_model_never_reaches_a_factory_without_selection_support() {
    let mut registry = ManagedProviderRegistry::default();
    let calls = add(&mut registry, "codex");
    let mut request = request("codex");
    request.delegation = Some(koru_session_protocol::DelegationLaunch {
        model: Some("private/model".parse().unwrap()),
        mcp_endpoint: "http://localhost:1/mcp".parse().unwrap(),
        credential: None,
        tools: Default::default(),
        resume: None,
    });
    assert_eq!(
        registry.create(request.clone()).err().unwrap().code,
        ErrorCode::CapabilityNotNegotiated
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let codex = CodexFactory::new(Arc::new(NoopAttachmentPathResolver));
    assert_eq!(
        codex.create(request).err().unwrap().code,
        ErrorCode::CapabilityNotNegotiated
    );
}
