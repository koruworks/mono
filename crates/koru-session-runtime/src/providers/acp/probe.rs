// SPDX-License-Identifier: MPL-2.0

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use koru_session_protocol::{ErrorCode, ProviderKind, SessionId};

use super::*;
use crate::AttachmentPathResolver;

/// Each request of the levels walk (D16) may take what a discovery
/// session's start does: setting Claude's model rebuilds its options.
const WALK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct AcpCompatibilityProbe {
    route: AcpRoute,
    executable: OsString,
    working_directory: PathBuf,
    attachment_paths: Arc<dyn AttachmentPathResolver>,
}

impl AcpCompatibilityProbe {
    pub fn new(
        mut route: AcpRoute,
        executable: impl Into<OsString>,
        working_directory: impl Into<PathBuf>,
        attachment_paths: Arc<dyn AttachmentPathResolver>,
    ) -> Self {
        route.enforce_environment();
        Self {
            route,
            executable: executable.into(),
            working_directory: working_directory.into(),
            attachment_paths,
        }
    }

    fn version_command(&self) -> Result<ProbeCommand, ProviderFailure> {
        let (executable, arguments) = self.route.launch(&self.executable, true)?;
        let command = arguments
            .iter()
            .fold(ProbeCommand::new(&executable), |command, argument| {
                command.arg(argument)
            });
        Ok(self
            .route
            .environment
            .iter()
            .fold(command, |command, (key, value)| command.env(key, value)))
    }

    pub(super) fn version(&self, limits: ProbeLimits) -> Result<ProviderVersion, ProviderFailure> {
        self.read_version(limits).inspect_err(|error| {
            diagnostics::probe_failure(&self.route, error);
        })
    }

    fn read_version(&self, limits: ProbeLimits) -> Result<ProviderVersion, ProviderFailure> {
        let rejected = || {
            failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            )
        };
        let version = if self.route.dialect.identified_by_digest() {
            self.route
                .parse_version(&antigravity::executable_digest(&self.executable)?)?
        } else {
            let output = run_probe_command(self.version_command()?, limits)?;
            if !output.success() {
                return Err(rejected());
            }
            let text = std::str::from_utf8(output.stdout()).map_err(|_| rejected())?;
            self.route.parse_version(text)?
        };
        if !self.route.supports(&version) {
            return Err(rejected());
        }
        Ok(version)
    }
}

/// A root-owned factory for one reviewed ACP route; no registration side effect.
#[derive(Clone)]
pub struct AcpFactory {
    route: AcpRoute,
    attachment_paths: Arc<dyn AttachmentPathResolver>,
}

impl AcpFactory {
    pub fn new(route: AcpRoute, attachment_paths: Arc<dyn AttachmentPathResolver>) -> Self {
        Self {
            route,
            attachment_paths,
        }
    }

    /// **The installed copy this route runs.** `executable` as resolved,
    /// unless the route chooses by version (Devin) and `executable` is one
    /// of several copies of its program — then the first, in
    /// `resolve_executable`'s order, whose `--version` the route admits. A
    /// machine with Devin's self-updating CLI behind its desktop app's copy
    /// gets the app's. None qualifying keeps `executable`, whose version
    /// stage then says why; a path that is not one of the copies is kept.
    fn choose_executable(&self, executable: OsString) -> OsString {
        let Some(registration) = self.route.dialect.registration() else {
            return executable;
        };
        if !self.route.dialect.chooses_by_version() {
            return executable;
        }
        let candidates =
            koru_session_profiles::resolve_executable_candidates(registration.executable);
        if candidates.len() < 2
            || !candidates
                .iter()
                .any(|candidate| candidate.as_os_str() == executable)
        {
            return executable;
        }
        candidates
            .into_iter()
            .map(PathBuf::into_os_string)
            .find(|candidate| {
                AcpCompatibilityProbe::new(
                    self.route.clone(),
                    candidate.clone(),
                    PathBuf::new(),
                    Arc::clone(&self.attachment_paths),
                )
                .read_version(ProbeLimits::default())
                .is_ok()
            })
            .unwrap_or(executable)
    }
}

impl ManagedProviderFactory for AcpFactory {
    fn supports_model_selection(&self) -> bool {
        self.route.dialect != routes::AcpDialect::Generic
    }

    fn discover_models(
        &self,
        executable: OsString,
        working_directory: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        // An unresolved name is passed on as it came, so discovery reports
        // the missing program itself.
        let executable = crate::providers::resolve_probe_executable(&executable)
            .map(|resolved| self.choose_executable(resolved))
            .unwrap_or(executable);
        models::discover(
            &self.route,
            executable,
            working_directory,
            self.attachment_paths.clone(),
        )
    }

    fn catalog_source(&self) -> CatalogSource {
        self.route.dialect.catalog_source()
    }

    /// Claude's levels belong to each model, and its session describes only
    /// the one it is on (D16).
    fn walks_model_levels(&self) -> bool {
        self.route.is_claude()
    }

    fn walk_model_levels(
        &self,
        executable: OsString,
        working_directory: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        let executable =
            self.choose_executable(crate::providers::resolve_probe_executable(&executable)?);
        let probe = AcpCompatibilityProbe::new(
            self.route.clone(),
            executable,
            working_directory,
            Arc::clone(&self.attachment_paths),
        );
        let models = probe.walk_levels(ProbeLimits {
            timeout: WALK_TIMEOUT,
            ..ProbeLimits::default()
        })?;
        koru_session_protocol::ProviderModels::try_from(models).map_err(|_| {
            failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            )
        })
    }

    fn compatibility_probe(
        &self,
        executable: OsString,
        working_directory: PathBuf,
    ) -> Result<Box<dyn CompatibilityProbe>, ProviderFailure> {
        let executable =
            self.choose_executable(crate::providers::resolve_probe_executable(&executable)?);
        Ok(Box::new(AcpCompatibilityProbe::new(
            self.route.clone(),
            executable,
            working_directory,
            Arc::clone(&self.attachment_paths),
        )))
    }

    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        let explicit_model = request
            .delegation
            .as_ref()
            .is_some_and(|launch| launch.model.is_some())
            || request
                .selection
                .as_ref()
                .is_some_and(|selection| selection.model.is_some());
        // An explicit effort is applied at start through the session's
        // `thought_level` option, and a route that offers none refuses it
        // there (agent-models D18, D19).
        if explicit_model && !self.supports_model_selection() {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        if request.provider != self.route.provider {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        let executable = self.choose_executable(request.executable);
        let probe = AcpCompatibilityProbe::new(
            self.route.clone(),
            executable.clone(),
            request.working_directory.clone(),
            self.attachment_paths.clone(),
        );
        let version = probe.version(ProbeLimits::default())?;
        let config = AcpConfig::new(
            self.route.clone(),
            executable,
            request.working_directory,
            version,
            self.attachment_paths.clone(),
        )?
        .with_delegation(request.delegation)
        .with_selection(request.selection)
        .with_mcp_servers(request.mcp_servers);
        let wake: Arc<dyn ProviderWake> = config.wake.clone();
        Ok(ManagedProvider {
            control: Box::new(AcpControl::new(config)),
            wake,
        })
    }
}

impl AcpCompatibilityProbe {
    /// **The levels walk** (D16): a discovery session that sets its own model
    /// to each listed one and keeps the levels each answer carries, then
    /// shuts down. No prompt is sent. A walk cut short keeps what it read.
    pub(super) fn walk_levels(
        &self,
        limits: ProbeLimits,
    ) -> Result<Vec<koru_session_protocol::ProviderModel>, ProviderFailure> {
        let version = self.version(limits)?;
        let config = AcpConfig::new(
            self.route.clone(),
            self.executable.clone(),
            self.working_directory.clone(),
            version,
            self.attachment_paths.clone(),
        )?
        .with_limits(limits)?;
        let mut control = AcpControl::new(config);
        let result = control.start(AgentStart::fresh(SessionId::mint(), None));
        let models = result.as_ref().ok().map(|_| control.walk_levels());
        let _ = control.shutdown();
        result?;
        Ok(models.unwrap_or_default())
    }

    /// A discovery session: the same start as a chat's, kept only for what
    /// it advertised under `model`, then shut down. No prompt is sent.
    pub(super) fn session_models(
        &self,
        limits: ProbeLimits,
    ) -> Result<Vec<koru_session_protocol::ProviderModel>, ProviderFailure> {
        let version = self.version(limits)?;
        let config = AcpConfig::new(
            self.route.clone(),
            self.executable.clone(),
            self.working_directory.clone(),
            version,
            self.attachment_paths.clone(),
        )?
        .with_limits(limits)?;
        let mut control = AcpControl::new(config);
        let result = control.start(AgentStart::fresh(SessionId::mint(), None));
        let models = control.model_options().to_vec();
        let shutdown = control.shutdown();
        result?;
        shutdown?;
        Ok(models)
    }
}

impl CompatibilityProbe for AcpCompatibilityProbe {
    fn provider(&self) -> ProviderKind {
        self.route.provider.clone()
    }

    fn raw_available(&self) -> bool {
        // Wrapper readiness never answers whether a separate raw CLI exists;
        // the host retains raw presence from the raw profile's own probe.
        // A server with no version flag has nothing to run: its presence is
        // the file; which build it is stays the version stage's question.
        if self.route.dialect.identified_by_digest() {
            return self
                .executable
                .to_str()
                .and_then(koru_session_profiles::resolve_executable)
                .is_some();
        }
        self.version_command()
            .and_then(|command| run_probe_command(command, ProbeLimits::default()))
            .is_ok_and(|output| output.success())
    }

    fn probe(&self, limits: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        self.probe_with_models(limits)
            .map(|(capabilities, _)| capabilities)
    }

    /// The same start a chat's session makes; its `model` option's values
    /// are kept when this route's catalog comes from the probe, rather than
    /// thrown away and asked for again by a discovery session.
    fn probe_with_models(
        &self,
        limits: ProbeLimits,
    ) -> Result<
        (
            AgentCapabilities,
            Option<koru_session_protocol::ProviderModels>,
        ),
        ProviderFailure,
    > {
        self.probe_with_capture(limits)
            .map(|(capabilities, capture)| (capabilities, capture.models))
    }

    /// The same start, keeping its `model` values as
    /// [`probe_with_models`](Self::probe_with_models) does and, on every
    /// route, the approval modes it advertised (koru-ai D7).
    fn probe_with_capture(
        &self,
        limits: ProbeLimits,
    ) -> Result<(AgentCapabilities, ProbeCapture), ProviderFailure> {
        let version = self.version(limits)?;
        let config = AcpConfig::new(
            self.route.clone(),
            self.executable.clone(),
            self.working_directory.clone(),
            version,
            self.attachment_paths.clone(),
        )?
        .with_limits(limits)?;
        let mut control = AcpControl::new(config);
        let result = control.start(AgentStart::fresh(SessionId::mint(), None));
        let models = (self.route.dialect.catalog_source() == CatalogSource::Probe)
            .then(|| {
                koru_session_protocol::ProviderModels::try_from(control.model_options().to_vec())
                    .ok()
            })
            .flatten();
        let modes = control.modes().cloned();
        let shutdown = control.shutdown();
        let capabilities = result?;
        shutdown?;
        Ok((capabilities, ProbeCapture { models, modes }))
    }
}
